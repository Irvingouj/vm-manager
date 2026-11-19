# Installing the platform

This takes you from a Linux machine with Docker to a person signed in. The platform is one container image, `vibe`, started by `compose.yaml` beside a Postgres 18 and a reverse proxy that terminates TLS.

You need:

- a machine with Docker and its Compose plugin (`docker compose version` answers);
- a domain for the installation, here `vibe.example.com`, whose names all reach the machine: Applications are at `<name>.vibe.example.com`, the platform's API at `api.vibe.example.com`, sign-in at `auth.vibe.example.com`, the Dashboard at `dashboard.vibe.example.com` (a DNS record for `*.vibe.example.com`);
- a certificate for `*.vibe.example.com` and its private key, in PEM;
- an OIDC identity provider at which you can register a client;
- the tool `vibe` on the computer you connect OIDC from.

Replace `vibe.example.com` with your domain in every command below.

## 1. The files

Make a folder for the installation and put the files of this folder in it: `compose.yaml`, `compose.own-proxy.yaml`, `compose.own-postgres.yaml` and `.env.example`, and the two folders `ca/` and `certs/`.

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
- Without one of the three, `docker compose` refuses to start and names the value.

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

## Your own reverse proxy

Instead of the proxy of `compose.yaml`:

```sh
docker compose -f compose.yaml -f compose.own-proxy.yaml up -d
```

There is no `proxy` container then, and the program's own port is published, plain HTTP, at `VIBE_HTTP_BIND:VIBE_HTTP_PORT` (`127.0.0.1:8080` without them), for your proxy only. Your proxy terminates TLS for `*.vibe.example.com` and must pass `Host` on unchanged. Set `VIBE_HTTPS_PORT` to the port your proxy is reached at when it is not 443: the addresses the platform writes come from it, never from a header of the request.

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

removes the bundled Postgres's data, every Application's included, for good.

## How it is hardened

- `vibe` runs with a read-only root filesystem, every capability dropped, `no-new-privileges`, as the image's user `65532` (not root), with no writable volume and no tmpfs; its one mount is `ca/`, read-only, at `/etc/vibe/ca`. The installation's key and the database's address are in its environment, in no command line.
- `postgres` runs as its image's user `999` and `proxy` as `65532`, neither as root, each with every capability dropped and `no-new-privileges`.
- Not possible: a read-only root filesystem for `postgres` and `proxy`. Each takes a file of `compose.yaml`'s `configs` (Postgres's first-start script, the proxy's configuration), and Compose cannot place such a file into a read-only container.
