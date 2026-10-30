# Domain Connect

Domain Connect lets a workspace add its sending domain's DNS records in one step at its DNS provider, instead of copying them by hand. It only works at providers that have taken in OnHold's template; everywhere else the settings page lists the records to add by hand.

`onhold.example.sending.json` is the template. Before it is submitted, replace `onhold.example` with OnHold's real domain everywhere (the file name too):

- `providerId`: OnHold's domain. It goes in `DOMAIN_CONNECT_PROVIDER_ID`.
- `syncPubKeyDomain`: where the public key is published (below).
- `syncRedirectDomain`: the web app's host. Providers send people back only there, to `/settings/email/domain`.

The record names (`resend._domainkey`, `send`) and the SPF value are Resend's. OnHold offers the one-step setup only when the records Resend gives a domain have exactly these names and that value, so a change on Resend's side falls back to the manual list rather than adding wrong records.

## Setting it up

1. Make a signing key:

   ```sh
   openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out domain-connect.pem
   openssl rsa -in domain-connect.pem -pubout -outform DER | base64 | tr -d '\n'
   ```

2. Publish the public key (the second command's output) as TXT records at `_dck1.<syncPubKeyDomain>`, split into pieces under 255 characters: `p=1,a=RS256,d=<first piece>`, `p=2,a=RS256,d=<second piece>`, and so on.
3. Set `DOMAIN_CONNECT_PROVIDER_ID` and `DOMAIN_CONNECT_PRIVATE_KEY` (the PEM; on one line with `\n` is fine). `DOMAIN_CONNECT_KEY_HOST` defaults to `_dck1`, `DOMAIN_CONNECT_SERVICE_ID` to `sending`. Domain Connect needs `RESEND_API_KEY` as well.
4. Check a link before submitting the template. Add a domain, open "Set up automatically", copy the link, and paste its query string and `sig` into the Domain Connect example service's signature checker, with key `_dck1` on the `syncPubKeyDomain`. OnHold signs the query string before `&sig=` with RSA-SHA256 (PKCS#1 v1.5) and sends the signature base64-encoded.
5. Lint the template with `dc-template-linter`, then submit it to the Domain-Connect Templates repository. Each DNS provider takes in templates from there on its own schedule.
