# Installing the platform

This takes you from a Linux machine with Docker to a person signed in. The platform is one container image, `vibe`, started by `compose.yaml` beside a Postgres 18 and a reverse proxy that terminates TLS.

You need:

- a machine with Docker Engine 25.0 or later and its Compose plugin 2.24.1 or later: `docker version --format '{{.Server.Version}}'` prints the Engine's version, `docker compose version` the plugin's. That minimum is read from Docker's notes and was not run; the install tests ran with Engine 28.0.4 and Compose 2.38.2, and with Engine 29.2.1 and Compose 5.0.2 (see [Why these versions of Docker](#why-these-versions-of-docker), at the end);
- a domain for the installation, here `vibe.example.com`, whose names all reach the machine: Applications are at `<name>.vibe.example.com`, the platform's API at `api.vibe.example.com`, sign-in at `auth.vibe.example.com`, the Dashboard at `dashboard.vibe.example.com` (a DNS record for `*.vibe.example.com`);
- a certificate for `*.vibe.example.com` and its private key, in PEM;
- an OIDC identity provider at which you can register a client;
- the tool `vibe` on the computer you connect OIDC from.

Replace `vibe.example.com` with your domain in every command below.

## 1. The files

Make a folder for the installation and put the files of this folder in it: `compose.yaml`, `compose.own-proxy.yaml`, `compose.own-tls.yaml`, `compose.own-postgres.yaml` and `.env.example`, and the two folders `ca/` and `certs/`.

```sh
mkdir -p /opt/vibe/ca /opt/vibe/certs
cd /opt/vibe
```

Every command below is run in this folder.

## 2. The values

`compose.yaml` reads its values from `.env` beside it. `.env.example` lists every value and what it is for. Three are required: the domain, the installation's key and the bundled Postgres's password.

```sh
cp .env.example .env
chmod 600 .env
sed -i "s|^VIBE_DOMAIN=.*|VIBE_DOMAIN=vibe.example.com|" .env
sed -i "s|^VIBE_SECRET_KEY=.*|VIBE_SECRET_KEY=$(openssl rand -base64 32)|" .env
sed -i "s|^POSTGRES_PASSWORD=.*|POSTGRES_PASSWORD=$(openssl rand -hex 24)|" .env
```

- `VIBE_SECRET_KEY` is the installation's key (`openssl rand -base64 32`). It seals the OIDC connection and the sessions in the platform database. Keep `.env` as long as you keep the data: with another key, OIDC has to be connected again.
- `POSTGRES_PASSWORD` is the password of the role `vibe` in the bundled Postgres (`openssl rand -hex 24`).
- Without `VIBE_DOMAIN` or `VIBE_SECRET_KEY`, `docker compose` refuses to start and names the value.
- Without `POSTGRES_PASSWORD`, or with it empty, the bundled Postgres does not start: `docker compose up -d` ends with an error (`dependency failed to start: container ... is unhealthy`), `vibe` is not started, `docker compose ps` shows `postgres` restarting, and `docker compose logs postgres` names `POSTGRES_PASSWORD`. If the value is removed later, from a Postgres already made, the Postgres starts but is never healthy and `vibe` is not started again: its health check says `POSTGRES_PASSWORD is empty in .env` (`docker inspect --format '{{json .State.Health.Log}}' $(docker compose ps --quiet postgres)`). Either way, set it in `.env` and run `docker compose up -d` again.

As downloaded, the installation is published on `127.0.0.1` only: it is reached from this machine alone. To let other machines in, set:

```sh
echo "VIBE_BIND=0.0.0.0" >> .env
```

It listens on port 443. For another port, set `VIBE_HTTPS_PORT` (for example `echo "VIBE_HTTPS_PORT=8443" >> .env`); every address the platform writes then carries that port.

## 3. The certificate

Put your certificate for `*.vibe.example.com`, followed by its intermediate certificates, in `certs/fullchain.pem`, and its private key in `certs/privkey.pem`. The proxy runs as the user `65532`, not as root; make the key readable by that user and nobody else:

```sh
cp /path/to/fullchain.pem certs/fullchain.pem
cp /path/to/privkey.pem certs/privkey.pem
chmod 644 certs/fullchain.pem
sudo chown 65532 certs/privkey.pem
sudo chmod 600 certs/privkey.pem
```

The proxy speaks TLS 1.2 and 1.3 with this certificate and passes every request on to the platform with its `Host` unchanged. It sets no limit of its own on a request's size or time: the platform's own apply (a request body of at most 16 MiB).

When the proxy cannot read the key, it does not start: `docker compose ps` shows it restarting, and `docker compose logs proxy` says `open /etc/caddy/certs/privkey.pem: permission denied`. Give the key to the user `65532` as above and run `docker compose restart proxy`.

With no proxy at all, the program can serve this certificate itself: see [The program serves TLS itself](#the-program-serves-tls-itself).

## 4. Start it

```sh
docker compose up -d
```

This starts three containers: `postgres` (the bundled Postgres 18, its data in the volume `postgres-data`, published nowhere), `vibe` (the program, published nowhere) and `proxy` (published on `VIBE_BIND:VIBE_HTTPS_PORT`). The first start of Postgres makes the role `vibe` and the platform database `vibe`; `vibe` starts once that first start has finished and Postgres takes connections (`docker compose ps` shows `postgres` as `healthy`).

Only HTTPS is served, on the published port alone. Nothing listens for plain `http://` (port 80 is not published) and nothing redirects it to `https://`; a plain HTTP request sent to the published port is refused with `400`. If you want `http://` addresses to be redirected, do it in a proxy of your own (see "Your own reverse proxy").

Check that it answers:

```sh
docker compose ps
curl https://api.vibe.example.com/v1/installation
```

The answer is `{"domain":"vibe.example.com","sign_in":"none","sign_in_address":null}`: no sign-in is connected yet. The program's log is the `vibe` container's: `docker compose logs vibe`.

## 5. The setup credential

```sh
docker compose exec vibe vibe setup
```

This prints the setup credential (`vmsc_...`) once, on your terminal and in no log. It is good for 24 hours and for one thing: connecting OIDC, once. Running it again ends the earlier one.

## 6. Register the platform at the identity provider

At your identity provider, register a client for the platform (an OpenID Connect client with a client secret, the authorization code flow):

- the redirect address: `https://auth.vibe.example.com/callback`; with `VIBE_HTTPS_PORT` other than 443, the port is in it: `https://auth.vibe.example.com:8443/callback`;
- the scopes: `openid profile email offline_access` (another list can be given to `vibe admin connect-oidc` with `--scopes`);
- a refresh token for the platform: the provider must allow `offline_access` for this client and for every person who signs in, or the sign-in fails. In Keycloak, for example, a person needs its role `offline_access`: people made in its console have it by default, people made by a realm import may not, and for them the sign-in ended on the platform's page with "The company's sign-in refused the code it gave" and the log record `identity-provider-refused` with the error `not_allowed`;
- the person's Groups in the ID token, in the claim `groups` (another claim can be given with `--groups-claim`);
- the ID token signed with `RS256` or `ES256`.

Note the issuer, the client id and the client secret, and choose the Group whose members are Platform administrators.

## 7. Connect OIDC

With the tool, from any computer that reaches the installation over HTTPS:

```sh
read -rs VIBE_SETUP_CREDENTIAL && export VIBE_SETUP_CREDENTIAL
read -rs VIBE_OIDC_CLIENT_SECRET && export VIBE_OIDC_CLIENT_SECRET
vibe admin connect-oidc --domain vibe.example.com \
    --issuer https://idp.example.com \
    --client-id vibe \
    --administrators-group vibe-administrators
```

The two `read` lines ask for the setup credential and the client secret without showing them or keeping them in the shell's history. With `VIBE_HTTPS_PORT` other than 443, add `--port <the port>`; while the domain is not in DNS yet, `--resolve <the machine's address>` connects there in its place.

The platform asks the identity provider for its configuration before it takes the connection; when it cannot use it, the command says why and nothing is changed. When it is taken, `curl https://api.vibe.example.com/v1/installation` says `"sign_in":"oidc"`.

## 8. Sign in

In a browser, open `https://dashboard.vibe.example.com/`: it sends you to the identity provider and back, signed in. The tool signs in through the browser:

```sh
vibe sign-in --domain vibe.example.com
vibe whoami
```

An Application can be shared with someone by their email address once that person has signed in here once; before that the platform refuses (`unknown-email`, and the Dashboard says nobody who signed in here has that email address).

### When a sign-in fails

The platform's page says in one sentence what failed, and the program's log has the record of it (`docker compose logs vibe`):

- `{"event":"identity-provider-refused","status",...,"error",...}`: the identity provider refused the platform's call to its token endpoint; `error` is the provider's own error code (never its description), for example `not_allowed` when it does not allow `offline_access` (step 6);
- `{"event":"identity-provider-unanswered","call",...,"problem",...}`: a call to the provider brought no answer the platform could use; `call` is `configuration`, `keys` or `token`, `problem` says what was wrong (`unreachable`, `timed-out`, `too-large`, `status`, `not-json`, `no-id-token`, `another-issuer`, `lacks-endpoint`, `unreadable-keys`), with the answer's `status` when it had one.

The person's browser and the tool's computer must trust the installation's certificate: one from a public authority, or the company's own authority installed on them (for the tool, `VIBE_CA_FILE`: see [Your company's own certificate authority](#your-companys-own-certificate-authority)).

## Your own reverse proxy

Instead of the proxy of `compose.yaml`:

```sh
docker compose -f compose.yaml -f compose.own-proxy.yaml up -d
```

There is no `proxy` container then, and the program's own port is published, plain HTTP, at `VIBE_HTTP_BIND:VIBE_HTTP_PORT` (`127.0.0.1:8080` without them), for your proxy only. Your proxy terminates TLS for `*.vibe.example.com` and must pass `Host` on unchanged. Set `VIBE_HTTPS_PORT` to the port your proxy is reached at when it is not 443: the addresses the platform writes come from it, never from a header of the request.

## The program serves TLS itself

Instead of the proxy of `compose.yaml`, for an installation that wants no proxy before the program: `vibe` itself ends TLS with the certificate and key of `certs/` (step 3) and is published at `VIBE_BIND:VIBE_HTTPS_PORT` (`127.0.0.1:443` without them).

```sh
docker compose -f compose.yaml -f compose.own-tls.yaml up -d
```

There is no `proxy` container then. `vibe` reads `certs/fullchain.pem` (your certificate for `*.vibe.example.com` first, its intermediate certificates after it, at most ten certificates) and `certs/privkey.pem` (its key, not encrypted: `PRIVATE KEY`, `RSA PRIVATE KEY` or `EC PRIVATE KEY`; an RSA key of 2048 to 4096 bits, an ECDSA key on P-256 or P-384, or an Ed25519 key, the kinds the program takes), mounted read-only at `/etc/vibe/certs`. It runs as the image's user `65532`, not as root, so the key must be readable by that user:

```sh
sudo chown 65532 certs/privkey.pem
sudo chmod 600 certs/privkey.pem
```

It serves HTTPS only on that port (TLS 1.2 and 1.3, HTTP/1.1); a plain HTTP request to it gets no answer and its connection is closed. Everything the platform writes is the same as behind the proxy: the addresses come from `VIBE_HTTPS_PORT`.

The program checks the two files when it starts, and does not start when they cannot be used: a key it cannot read, a key that is not the certificate's, a certificate that does not name `*.vibe.example.com` among its DNS names, an encrypted key (a passphrase is not supported). `docker compose ps` shows `vibe` restarting, and `docker compose logs vibe` names the variable (`VIBE_TLS_CERT_FILE` or `VIBE_TLS_KEY_FILE`), the file and why; it never shows anything of the key. A certificate that has expired is served all the same (a browser says so), and the log says `"valid_now":false`.

Replacing the certificate needs no restart. Copy the new files over the old ones, the key last (`cp new-fullchain.pem certs/fullchain.pem`, then `cp new-privkey.pem certs/privkey.pem`), or put both in place by renaming (`mv`), each from a file in `certs/`. The program looks at the two files every 5 seconds and gives the new certificate to new connections within 10 seconds of the change; connections already open keep the old one. `docker compose logs vibe` then shows `tls-certificate-loaded` with the new certificate's names, dates and fingerprint. A new pair that does not pass (a certificate whose key is not there yet, a certificate for another domain) changes nothing: the certificate in use goes on being served, and the log says why once, in a `tls-certificate-refused` record.

Beside the proxy it lacks HTTP/2: the program speaks HTTP/1.1 only. Nothing listens on port 80 either, as with the proxy.

`compose.own-tls.yaml` cannot be given together with `compose.own-proxy.yaml`: each replaces the proxy of `compose.yaml` with something else, and only one of them can. With `compose.own-postgres.yaml` it can: `docker compose -f compose.yaml -f compose.own-tls.yaml -f compose.own-postgres.yaml up -d`.

## Your own Postgres

Instead of the bundled one: your Postgres 18 (no other major version is taken; the `vibe` container's log says so and nothing is written). Do two things on it first, as its superuser:

1. Make a role for the platform that may create databases and roles and is not a superuser, and the platform database owned by it:

   ```sql
   CREATE ROLE vibe LOGIN CREATEDB CREATEROLE PASSWORD '<a password>';
   CREATE DATABASE vibe OWNER vibe;
   ```

2. Take `CONNECT` on the databases `postgres` and `template1` from `PUBLIC`. Every role may connect to them otherwise, the roles the platform makes for Applications' own databases included, and the platform, which is no superuser, cannot take it away itself:

   ```sql
   REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;
   REVOKE CONNECT ON DATABASE template1 FROM PUBLIC;
   ```

Then set its address in `.env` and start with the second file:

```sh
echo "VIBE_DATABASE_URL=postgres://vibe:<the password>@db.example.com:5432/vibe" >> .env
docker compose -f compose.yaml -f compose.own-postgres.yaml up -d
```

There is no `postgres` container and no volume then, and `POSTGRES_PASSWORD` is not needed. The two other files can be given together: `docker compose -f compose.yaml -f compose.own-proxy.yaml -f compose.own-postgres.yaml up -d`. Every later `docker compose` command takes the same `-f` files.

## Your company's own certificate authority

When your identity provider's certificate is issued by your company's own certificate authority, the platform has to trust it. Put the authority's certificate (PEM, one or more certificates, at most 1 MiB) in `ca/` and name it as the container sees it:

```sh
cp /path/to/company-ca.pem ca/company-ca.pem
chmod 644 ca/company-ca.pem
echo "VIBE_CA_FILE=/etc/vibe/ca/company-ca.pem" >> .env
docker compose up -d
```

It is trusted beside the public authorities built into the program, read when the program starts. The tool takes the same variable: when the installation's own certificate is issued by such an authority, set `VIBE_CA_FILE=/path/to/company-ca.pem` in the tool's environment on each computer that uses it.

## Storage

A Storage Resource is a shared folder of files that Applications read and write through the SDK, as the User of the request. Each one is a folder on the host, beneath the one folder `vibe` sees at `/var/lib/vibe/storage`, so the files are yours to inspect and back up. The program writes them as its user `65532` (files mode `0640`, folders `0750`), so another user of the host reads them with `sudo`.

`compose.yaml` mounts the host folder `VIBE_STORAGE_DIR` names, `./storage` beside `compose.yaml` without it, read-write at `/var/lib/vibe/storage`, and starts `vibe` with `--storage-root /var/lib/vibe/storage`. Compose makes that folder when it is not there, as root. `vibe` runs as the image's user `65532`, with every capability dropped, and writes only what that user owns. Make the folder and give it to the image's user:

```sh
mkdir -p storage && sudo chown 65532:65532 storage
```

Make one folder in it for each Storage Resource, as the image's user:

```sh
sudo -u '#65532' mkdir storage/marketing
```

That fails with `Permission denied` when the user `65532` cannot walk the path down to `storage/` (an install folder under a home directory of mode `0750` or `0700`, Ubuntu's default, for example); the other form, `mkdir storage/marketing && sudo chown 65532:65532 storage/marketing`, then makes the same folder, and it is `65532`'s and writable by it either way.

A Platform administrator registers the folder as `/var/lib/vibe/storage/marketing`, the path as the container sees it, with `"kind": "storage"` and `"location": { "backend": "directory", "directory": "/var/lib/vibe/storage/marketing" }` at the platform's API (`POST /v1/resources`), then grants Groups `read` or `write` on it.

Until the folder is made and given to the image's user, a Storage registration is refused: `directory-missing` when the folder is not there, and `directory-not-writable` while `storage/` is the wrong user's (root's with mode `0755`, made by a plain `mkdir`, for example). The installation comes up all the same, and its log says `storage-root-unusable` when `storage/` itself is not there (`docker compose logs vibe`).

The Storage folder is the one place the `vibe` process can write. A process that escaped the Application sandbox (a risk the platform accepts) could read, change and remove the files of every Storage Resource without a Grant, because the Grants are enforced by that same process, and could fill the file system the folder is on. So name a folder used for nothing but Storage (never a parent such as `/srv` or a home directory: `vibe` gets the whole tree read-write), put it on a file system or under a quota of its own where filling it cannot stop Postgres or the host, and treat what is in it as untrusted when you handle it as root: `tar` keeps a link as a link, and a recursive copy or a `chown -R` over the folder acts on whatever is there.

The files are at `storage/marketing/<key>` on the host, under the key the Application gave each one. Back the folder up with the host's own tools, as any folder of the host is backed up, while `vibe` runs or after `docker compose down` (which keeps it). For example:

```sh
sudo tar -czf storage-marketing-backup.tar.gz -C storage marketing
```

With another host folder, set it in `.env` and make it and give it to the image's user the same way:

```sh
echo "VIBE_STORAGE_DIR=/srv/vibe/storage" >> .env
mkdir -p /srv/vibe/storage && sudo chown 65532:65532 /srv/vibe/storage
docker compose up -d
```

## If the first start was interrupted

The first start of the bundled Postgres makes its data folder, then the role `vibe`, the platform database and the rest, and leaves a mark when it has finished. If the container was stopped or killed in between (the machine went down, `docker compose down` during the first `up`), Postgres starts again on what was made so far and skips the rest. It is then never healthy: `docker compose ps` shows `postgres` as `health: starting` for two minutes and as `unhealthy` after that, `vibe` is not started, and `docker compose up -d` ends with `dependency failed to start: container ... is unhealthy`. The health check says why:

```sh
docker compose ps
docker inspect --format '{{json .State.Health.Log}}' $(docker compose ps --quiet postgres)
```

Its output says `the first start of this Postgres did not finish`. Meanwhile you can look inside as the superuser: `docker compose exec postgres psql -U postgres`.

Nothing is in it yet, since `vibe` never started on it: remove it and start again.

```sh
docker compose down --volumes
docker compose up -d
```

## The bundled Postgres

The platform connects as the role `vibe`, never as a superuser. The superuser `postgres` cannot connect over the network at all, whatever password is given; you reach it inside the container:

```sh
docker compose exec postgres psql -U postgres
```

`CONNECT` on the databases `postgres` and `template1` is taken from `PUBLIC` at its first start.

## Upgrading

Set the newer image in `.env` and start again:

```sh
sed -i "s|^#\?VIBE_IMAGE=.*|VIBE_IMAGE=ghcr.io/irvingouj/vibe:<the newer version>|" .env
docker compose pull && docker compose up -d
```

The platform database, every Application's own database, the OIDC connection and the sessions are kept: the data is the volume's, the key is the `.env`'s.

## Stopping

```sh
docker compose down
```

stops and removes the containers and keeps the data; `docker compose up -d` brings it back as it was. Only

```sh
docker compose down --volumes
```

removes the bundled Postgres's data, every Application's included, for good. Neither command touches the Storage folder (see [Storage](#storage)): it is the host's, and only `rm` on the host removes it.

## How it is hardened

- `vibe` runs with a read-only root filesystem, every capability dropped, `no-new-privileges`, as the image's user `65532` (not root), and no tmpfs. Its mounts are `ca/`, read-only, at `/etc/vibe/ca`, and the Storage folder, read-write, at `/var/lib/vibe/storage` (with `compose.own-tls.yaml`, `certs/` too, read-only, at `/etc/vibe/certs`); the Storage folder is the one place it writes. The installation's key and the database's address are in its environment, in no command line.
- `vibe` may open 65,536 files (`ulimits: nofile`, soft and hard), whatever the Docker daemon's default, so that it never runs out of files before its own bound of 1,024 connections at once; started elsewhere, the program raises its own soft limit towards the hard one and logs `open-files-limit-low` when even the hard limit is below 4,096.
- `postgres` runs as its image's user `999` and `proxy` as `65532`, neither as root, each with every capability dropped and `no-new-privileges`.
- Not possible: a read-only root filesystem for `postgres` and `proxy`. Each takes a file of `compose.yaml`'s `configs` (Postgres's first-start script, the proxy's configuration), and Compose cannot place such a file into a read-only container.

## Why these versions of Docker

The install tests ran with two pairs of versions: Docker Engine 28.0.4 with Compose 2.38.2 (GitHub's hosted runner, in the release workflow), and Docker Engine 29.2.1 with Compose 5.0.2 (the machine of the install gate). The minimum this document gives, Engine 25.0 and Compose 2.24.1, was not run: it is read from Docker's notes. The Compose files use three things that older versions do not have, each according to Docker's own notes:

- a config written in the file itself (`configs: ... content:`), for the Postgres first-start script and the proxy's configuration: Compose 2.23.1 ([Compose file reference, configs](https://docs.docker.com/reference/compose-file/configs/): "`content`: The content is created with the inlined value. Introduced in Docker Compose version 2.23.1");
- the health check's `start_interval`, so that a fresh install is seen healthy within seconds: Docker Engine 25.0.0 ([Engine 25.0 release notes](https://docs.docker.com/engine/release-notes/25.0/), "Add an additional interval to be used by healthchecks during the container start period") and Compose 2.24.0, whose [release notes](https://github.com/docker/compose/releases/tag/v2.24.0) list "Add support for start_interval", with the check of the Engine's version it needs fixed in [2.24.1](https://github.com/docker/compose/releases/tag/v2.24.1) ("fix engine version require to use healthcheck.start_interval");
- `!reset`, by which `compose.own-proxy.yaml`, `compose.own-tls.yaml` and `compose.own-postgres.yaml` remove the bundled proxy or Postgres: compose-go [v1.13.5](https://github.com/compose-spec/compose-go/releases/tag/v1.13.5) ("allow override compose.yaml to REMOVE elements"), first in Compose 2.18.0 (its `go.mod` names compose-go v1.13.5), well below the others.
