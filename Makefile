# Slopstream server. Every target runs through docker compose; no host toolchain.
#
# Three compose files, three jobs:
#   docker/docker-compose-dev.yml    development + tests (bacon, Postgres)
#   docker/docker-compose-build.yml  build the production image LOCALLY and smoke-test it
#   deploy/docker-compose.yml        production example: released registry image behind Traefik
# CI runs on every push (.gitea/workflows/ci.yml); a v* tag publishes the
# image (.gitea/workflows/release.yml).
#
# Development
#   make dev           dev stack (bacon hot-reload + Postgres) in the foreground
#   make reset         wipe the dev database, restart dev
#   make test          full test suite (incl. Postgres integration)
#   make lint          rustfmt --check + clippy -D warnings (same gate as CI)
#   make fmt           rustfmt
#   make secret        print a fresh SLOPSTREAM_SECRET_KEY
#
# Local production-image smoke test (reads ./.env)
#   make image         build slopstream-server:local from docker/Dockerfile
#   make up / down     start / stop it with Postgres on ${PORT:-8080}
#   make logs
#   make admin EMAIL=you@example.com      (first account; then set its password)
#   make password EMAIL=you@example.com   (prompts, input not echoed)

DEV_COMPOSE   = docker compose -f docker/docker-compose-dev.yml
LOCAL_COMPOSE = docker compose --env-file .env -f docker/docker-compose-build.yml
DEV_RUN       = $(DEV_COMPOSE) run --rm --build slopstream-server

.PHONY: dev reset test lint fmt secret image up down logs admin password

dev:
	$(DEV_COMPOSE) up --build

reset:
	@printf "This wipes the DEV database. Continue? [y/N] " && read a && [ "$$a" = "y" ]
	$(DEV_COMPOSE) down -v
	$(DEV_COMPOSE) up --build

test:
	$(DEV_RUN) cargo test --workspace --release

lint:
	$(DEV_RUN) sh -c 'cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings'

fmt:
	$(DEV_RUN) cargo fmt --all

secret:
	@head -c 32 /dev/urandom | base64

image:
	$(LOCAL_COMPOSE) build

up:
	$(LOCAL_COMPOSE) up --build -d

down:
	$(DEV_COMPOSE) down
	$(LOCAL_COMPOSE) down

logs:
	$(LOCAL_COMPOSE) logs -f slopstream-server

admin:
	$(LOCAL_COMPOSE) exec slopstream-server slopstream-server create-admin $(EMAIL)

password:
	@stty -echo; $(LOCAL_COMPOSE) exec -T slopstream-server slopstream-server set-password $(EMAIL); status=$$?; stty echo; echo; exit $$status
