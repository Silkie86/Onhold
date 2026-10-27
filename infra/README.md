# Infra

- `Dockerfile`: the one image. `onhold api`, `onhold worker` and `onhold migrate` are subcommands of the same binary.
- `docker-compose.yml`: the dev stack (Postgres 16, a one-shot `migrate`, `api` on :8080, `worker` probes on :8081).

Deploy, backup and restore scripts arrive with the hosting decision (see `docs/DECISIONS.md`).
