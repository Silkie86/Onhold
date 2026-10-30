//! Google and Microsoft sign-in against a mock identity provider that signs
//! real RS256 ID tokens.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex, OnceLock};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use common::{Client, TestApp};
use core_api::Config;
use core_api::oidc::{OidcProvider, ProviderKind};
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    CoreRsaPrivateSigningKey,
};
use openidconnect::{
    Audience, EmptyAdditionalClaims, EndUserEmail, EndUserName, IssuerUrl, JsonWebKeyId,
    LocalizedClaim, Nonce, PrivateSigningKey, StandardClaims, SubjectIdentifier,
};
use serde_json::json;
use sqlx::PgPool;

const CLIENT_ID: &str = "onhold-test";

fn signing_key_pem() -> &'static str {
    static PEM: OnceLock<String> = OnceLock::new();
    PEM.get_or_init(|| {
        use rsa::pkcs1::EncodeRsaPrivateKey;
        let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        key.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF)
            .unwrap()
            .to_string()
    })
}

/// Who the mock provider says signed in, next time a code is exchanged.
#[derive(Clone, Default)]
struct NextLogin {
    subject: String,
    email: String,
    email_verified: bool,
    name: String,
    nonce: String,
}

struct Mock {
    issuer: String,
    key: CoreRsaPrivateSigningKey,
    next: Mutex<NextLogin>,
}

async fn mock_provider() -> Arc<Mock> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let key = CoreRsaPrivateSigningKey::from_pem(
        signing_key_pem(),
        Some(JsonWebKeyId::new("test".into())),
    )
    .unwrap();
    let mock = Arc::new(Mock {
        issuer,
        key,
        next: Mutex::default(),
    });

    let app = Router::new()
        .route("/.well-known/openid-configuration", get(|State(m): State<Arc<Mock>>| async move {
            axum::Json(json!({
                "issuer": m.issuer,
                "authorization_endpoint": format!("{}/authorize", m.issuer),
                "token_endpoint": format!("{}/token", m.issuer),
                "jwks_uri": format!("{}/jwks", m.issuer),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["RS256"],
            }))
        }))
        .route("/jwks", get(|State(m): State<Arc<Mock>>| async move {
            axum::Json(CoreJsonWebKeySet::new(vec![m.key.as_verification_key()]))
        }))
        .route("/token", post(|State(m): State<Arc<Mock>>, body: String| async move {
            // PKCE must be in use.
            assert!(body.contains("code_verifier="), "{body}");
            let next = m.next.lock().unwrap().clone();
            let mut name = LocalizedClaim::new();
            name.insert(None, EndUserName::new(next.name));
            let claims = CoreIdTokenClaims::new(
                IssuerUrl::new(m.issuer.clone()).unwrap(),
                vec![Audience::new(CLIENT_ID.into())],
                chrono::Utc::now() + chrono::Duration::minutes(5),
                chrono::Utc::now(),
                StandardClaims::new(SubjectIdentifier::new(next.subject))
                    .set_email(Some(EndUserEmail::new(next.email)))
                    .set_email_verified(Some(next.email_verified))
                    .set_name(Some(name)),
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(Nonce::new(next.nonce)));
            let id_token = CoreIdToken::new(claims, &m.key, CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256, None, None).unwrap();
            axum::Json(json!({ "access_token": "at", "token_type": "bearer", "expires_in": 300, "id_token": id_token }))
        }))
        .with_state(mock.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    mock
}

/// An app with two providers on the mock: `google` (trusts verified emails)
/// and `microsoft` (never does).
async fn app_with_oidc(pool: PgPool) -> (TestApp, Arc<Mock>) {
    let mock = mock_provider().await;
    let provider = |name: &str, trust: bool| OidcProvider {
        name: name.into(),
        display_name: name.into(),
        issuer: mock.issuer.clone(),
        client_id: CLIENT_ID.into(),
        client_secret: "shh".into(),
        kind: ProviderKind::Standard {
            trust_verified_email: trust,
        },
    };
    let config = Config {
        oidc: vec![provider("google", true), provider("microsoft", false)],
        ..Config::for_development()
    };
    (TestApp::with_config(pool, config).await, mock)
}

struct Outcome {
    status: StatusCode,
    location: url::Url,
    session: Option<Client>,
}

impl Outcome {
    fn error(&self) -> Option<String> {
        self.location
            .query_pairs()
            .find(|(k, _)| k == "error")
            .map(|(_, v)| v.into_owned())
    }
}

/// Runs the browser's side of a sign-in: start, "authenticate" at the
/// provider as `who`, come back to the callback.
async fn sign_in(
    app: &TestApp,
    mock: &Mock,
    provider: &str,
    query: &str,
    as_client: Option<&Client>,
    who: NextLogin,
) -> Outcome {
    let mut start = app.get(&format!("/api/v1/auth/oidc/{provider}/start{query}"));
    if let Some(c) = as_client {
        start = start.signed_in(c);
    }
    let res = start.send().await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "{}", res.body);
    let authorize =
        url::Url::parse(res.headers.get("location").unwrap().to_str().unwrap()).unwrap();
    assert!(
        authorize
            .as_str()
            .starts_with(&format!("{}/authorize", mock.issuer))
    );
    let param = |k: &str| {
        authorize
            .query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
            .unwrap()
    };
    assert_eq!(param("code_challenge_method"), "S256");
    let state_cookie = res
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|v| v.starts_with("onhold_oidc="))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    *mock.next.lock().unwrap() = NextLogin {
        nonce: param("nonce"),
        ..who
    };

    let mut cb = app
        .get(&format!(
            "/api/v1/auth/oidc/{provider}/callback?code=xyz&state={}",
            param("state")
        ))
        .header("cookie", &state_cookie);
    if let Some(c) = as_client {
        cb = cb.header("cookie", &format!("{}; {state_cookie}", c.cookie));
    }
    let res = cb.send().await;
    let location = url::Url::parse(res.headers.get("location").unwrap().to_str().unwrap()).unwrap();
    let session = res
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap())
        .find(|v| v.starts_with("onhold_session="))
        .map(|v| Client {
            cookie: v.split(';').next().unwrap().to_owned(),
            csrf: String::new(),
            workspace: None,
        });
    Outcome {
        status: res.status,
        location,
        session,
    }
}

fn person(subject: &str, email: &str, verified: bool) -> NextLogin {
    NextLogin {
        subject: subject.into(),
        email: email.into(),
        email_verified: verified,
        name: "Pat Example".into(),
        nonce: String::new(),
    }
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn providers_are_listed(pool: PgPool) {
    let (app, _) = app_with_oidc(pool).await;
    let res = app.get("/api/v1/auth/oidc/providers").send().await.ok();
    assert_eq!(
        res.body,
        json!([{ "name": "google", "display_name": "google" }, { "name": "microsoft", "display_name": "microsoft" }])
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_verified_google_email_signs_in_its_existing_account(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    app.workspace_with_owner("acme").await;

    let out = sign_in(
        &app,
        &mock,
        "google",
        "?redirect=/bookings%3Fview%3Dweek",
        None,
        person("g-1", "owner@acme.test", true),
    )
    .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    assert_eq!(
        out.location.as_str(),
        "http://localhost:5173/bookings?view=week"
    );
    let client = out.session.expect("signed in");
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&client)
        .send()
        .await
        .ok();
    assert_eq!(me.body["user"]["email"], "owner@acme.test");

    // Next time the identity itself is enough, whatever the email says.
    let out = sign_in(
        &app,
        &mock,
        "google",
        "",
        None,
        person("g-1", "renamed@elsewhere.test", false),
    )
    .await;
    assert!(out.session.is_some(), "{}", out.location);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn unverified_or_unknown_emails_get_no_account(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    app.workspace_with_owner("acme").await;

    let out = sign_in(
        &app,
        &mock,
        "google",
        "",
        None,
        person("g-2", "owner@acme.test", false),
    )
    .await;
    assert_eq!(
        (out.error().as_deref(), out.session.is_some()),
        (Some("no_account"), false)
    );
    let out = sign_in(
        &app,
        &mock,
        "google",
        "",
        None,
        person("g-3", "stranger@nowhere.test", true),
    )
    .await;
    assert_eq!(
        (out.error().as_deref(), out.session.is_some()),
        (Some("no_account"), false)
    );
    assert_eq!(out.location.path(), "/sign-in");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn microsoft_emails_are_never_trusted_to_match_accounts(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    app.workspace_with_owner("acme").await;
    // An attacker's tenant can claim any address, "verified" or not.
    let out = sign_in(
        &app,
        &mock,
        "microsoft",
        "",
        None,
        person("ms-evil", "owner@acme.test", true),
    )
    .await;
    assert_eq!(
        (out.error().as_deref(), out.session.is_some()),
        (Some("no_account"), false)
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_invitation_can_be_accepted_by_signing_in_with_a_provider(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    let (ws, owner) = app.workspace_with_owner("acme").await;
    app.post("/api/v1/invitations")
        .signed_in(&owner)
        .json(json!({ "email": "new@acme.test", "role": "agent" }))
        .send()
        .await
        .ok();
    let token = app.last_invitation_token(ws).await;

    // The provider's email differs: holding the invitation is what counts.
    let out = sign_in(
        &app,
        &mock,
        "microsoft",
        &format!("?invitation={token}"),
        None,
        person("ms-1", "n.ew@contoso.test", false),
    )
    .await;
    let client = out.session.expect("signed in").in_workspace(ws);
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&client)
        .send()
        .await
        .ok();
    assert_eq!(me.body["user"]["email"], "new@acme.test");
    assert_eq!(me.body["user"]["name"], "Pat Example");
    assert_eq!(me.body["user"]["has_password"], false);
    assert_eq!(me.body["workspaces"][0]["role"], "agent");
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_invitation_never_attaches_a_stranger_to_an_existing_account(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    app.workspace_with_owner("acme").await;
    // Whoever holds an invitation for an address that already has an account...
    let (beta, token) = app.create_workspace("beta", "owner@acme.test").await;

    // ...cannot sign in to that account with their own identity.
    for (provider, who) in [
        ("microsoft", person("ms-evil", "evil@contoso.test", false)),
        ("microsoft", person("ms-evil", "owner@acme.test", true)),
        ("google", person("g-evil", "evil@gmail.test", true)),
        ("google", person("g-evil", "owner@acme.test", false)),
    ] {
        let out = sign_in(
            &app,
            &mock,
            provider,
            &format!("?invitation={token}"),
            None,
            who,
        )
        .await;
        assert_eq!(
            (out.error().as_deref(), out.session.is_some()),
            (Some("account_exists"), false),
            "{provider}"
        );
    }
    let linked: i64 = sqlx::query_scalar("SELECT count(*) FROM oidc_identities")
        .fetch_one(&app.owner)
        .await
        .unwrap();
    assert_eq!(linked, 0, "nothing was linked");

    // A provider that vouches for that exact address may still use it.
    let out = sign_in(
        &app,
        &mock,
        "google",
        &format!("?invitation={token}"),
        None,
        person("g-1", "Owner@Acme.test", true),
    )
    .await;
    let client = out.session.expect("signed in").in_workspace(beta);
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&client)
        .send()
        .await
        .ok();
    assert_eq!(me.body["workspaces"].as_array().unwrap().len(), 2);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_signed_in_user_can_link_a_provider(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    let (_, owner) = app.workspace_with_owner("acme").await;

    let out = sign_in(
        &app,
        &mock,
        "microsoft",
        "?link=true&redirect=/settings",
        Some(&owner),
        person("ms-2", "whatever@x.test", false),
    )
    .await;
    assert_eq!(
        out.location.as_str(),
        "http://localhost:5173/settings?linked=microsoft"
    );
    assert!(out.session.is_none());

    let out = sign_in(
        &app,
        &mock,
        "microsoft",
        "",
        None,
        person("ms-2", "whatever@x.test", false),
    )
    .await;
    let client = out.session.expect("signed in through the linked identity");
    let me = app
        .get("/api/v1/auth/me")
        .signed_in(&client)
        .send()
        .await
        .ok();
    assert_eq!(me.body["user"]["email"], "owner@acme.test");

    // Linking requires being signed in.
    let res = app
        .get("/api/v1/auth/oidc/microsoft/start?link=true")
        .send()
        .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn an_identity_cannot_be_linked_to_two_users(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    let (_, alice) = app.workspace_with_owner("alpha").await;
    let (_, bob) = app.workspace_with_owner("bravo").await;
    sign_in(
        &app,
        &mock,
        "google",
        "?link=true",
        Some(&alice),
        person("g-shared", "a@x.test", true),
    )
    .await;
    let out = sign_in(
        &app,
        &mock,
        "google",
        "?link=true",
        Some(&bob),
        person("g-shared", "a@x.test", true),
    )
    .await;
    assert_eq!(out.error().as_deref(), Some("identity_in_use"));
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn forged_or_missing_flow_state_is_refused(pool: PgPool) {
    let (app, _) = app_with_oidc(pool).await;
    let res = app
        .get("/api/v1/auth/oidc/google/callback?code=x&state=y")
        .send()
        .await;
    let location = res.headers.get("location").unwrap().to_str().unwrap();
    assert!(
        location.ends_with("/sign-in?error=oidc_state_invalid"),
        "{location}"
    );

    let res = app
        .get("/api/v1/auth/oidc/google/callback?code=x&state=y")
        .header("cookie", "onhold_oidc=bm90IGVuY3J5cHRlZA")
        .send()
        .await;
    let location = res.headers.get("location").unwrap().to_str().unwrap();
    assert!(
        location.ends_with("/sign-in?error=oidc_state_invalid"),
        "{location}"
    );

    let res = app
        .get("/api/v1/auth/oidc/google/callback?error=access_denied")
        .send()
        .await;
    let location = res.headers.get("location").unwrap().to_str().unwrap();
    assert!(
        location.ends_with("/sign-in?error=oidc_cancelled"),
        "{location}"
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn a_token_for_another_nonce_is_refused(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    app.workspace_with_owner("acme").await;

    let res = app.get("/api/v1/auth/oidc/google/start").send().await;
    let authorize =
        url::Url::parse(res.headers.get("location").unwrap().to_str().unwrap()).unwrap();
    let state = authorize
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let cookie = res
        .headers
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    *mock.next.lock().unwrap() = NextLogin {
        nonce: "replayed-from-elsewhere".into(),
        ..person("g-1", "owner@acme.test", true)
    };

    let res = app
        .get(&format!(
            "/api/v1/auth/oidc/google/callback?code=x&state={state}"
        ))
        .header("cookie", &cookie)
        .send()
        .await;
    let location = res.headers.get("location").unwrap().to_str().unwrap();
    assert!(
        location.ends_with("/sign-in?error=oidc_failed"),
        "{location}"
    );
    assert!(
        res.headers
            .get_all("set-cookie")
            .iter()
            .all(|v| !v.to_str().unwrap().starts_with("onhold_session="))
    );
}

#[sqlx::test(migrator = "core_db::MIGRATOR")]
async fn redirects_never_leave_the_site(pool: PgPool) {
    let (app, mock) = app_with_oidc(pool).await;
    app.workspace_with_owner("acme").await;
    let out = sign_in(
        &app,
        &mock,
        "google",
        "?redirect=https://evil.test/",
        None,
        person("g-1", "owner@acme.test", true),
    )
    .await;
    assert_eq!(out.location.as_str(), "http://localhost:5173/");
}
