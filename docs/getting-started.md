# Getting started

This page takes you from nothing to an Application open in your browser. It uses the files of the release `v0.1.0` only. Read the [release's notes](releases/v0.1.0.md) first: this release is not for untrusted Applications or real company data.

There are two people in this page. They can be the same person.

- The **administrator** installs the platform on a server and connects it to the company's sign-in (OIDC).
- The **Builder** installs the tool `vibe` on their own computer, makes an Application, tries it there, deploys it and shares it.

Every command below uses the domain `vibe.example.com`. Put your own domain in its place.

## What you need

For the installation:

- a Linux server with Docker Engine 25.0 or later and its Compose plugin 2.24.1 or later, on `x86-64` (the image is for `linux/amd64` only). That minimum is read from Docker's notes and was not tried; [`deploy/README.md`](../deploy/README.md) names the versions the install tests ran with;
- a domain whose names all reach the server: a DNS record for `*.vibe.example.com`. Applications are at `<name>.vibe.example.com`;
- a certificate for `*.vibe.example.com` and its private key, in PEM;
- an OIDC identity provider at which you can register a client.

For the Builder's computer:

- Linux on `x86-64` with glibc 2.35 or later (Ubuntu 22.04, Debian 12 and later), or macOS on Apple silicon;
- Node.js 22 or later, with `npm`;
- `git`.

## 1. Install the platform

On the server, make a folder and download the release's files into it:

```sh
mkdir -p /opt/vibe/certs /opt/vibe/ca
cd /opt/vibe
curl -fLO https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0/compose.yaml
curl -fLO https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0/env.example
curl -fLO https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
```

The last command checks the files you downloaded. Each line must end with `OK`.

Make the file of values, `.env`, from `env.example`. Three values are required: the domain, the installation's key and the password of the bundled Postgres.

```sh
cp env.example .env
chmod 600 .env
sed -i "s|^VIBE_DOMAIN=.*|VIBE_DOMAIN=vibe.example.com|" .env
sed -i "s|^VIBE_SECRET_KEY=.*|VIBE_SECRET_KEY=$(openssl rand -base64 32)|" .env
sed -i "s|^POSTGRES_PASSWORD=.*|POSTGRES_PASSWORD=$(openssl rand -hex 24)|" .env
```

Without the domain or the key, `docker compose` refuses to start and names the value. Without the password, the bundled Postgres does not start and `docker compose logs postgres` names `POSTGRES_PASSWORD`: set it and run `docker compose up -d` again. Keep `.env` as long as you keep the data. As downloaded, the installation answers on `127.0.0.1` only. To let other machines in, add `VIBE_BIND=0.0.0.0`:

```sh
echo "VIBE_BIND=0.0.0.0" >> .env
```

Put the certificate in `certs/`. The proxy runs as the user `65532`, so give the key to that user:

```sh
cp /path/to/fullchain.pem certs/fullchain.pem
cp /path/to/privkey.pem certs/privkey.pem
chmod 644 certs/fullchain.pem
sudo chown 65532 certs/privkey.pem
sudo chmod 600 certs/privkey.pem
```

Start it, and check that it answers:

```sh
docker compose up -d
docker compose ps
curl https://api.vibe.example.com/v1/installation
```

The answer says `"sign_in":"none"`: no sign-in is connected yet.

The install document, [`deploy/README.md`](../deploy/README.md), says more about each step: another port, your own reverse proxy, your own Postgres, your company's own certificate authority, upgrading, and what to do when something fails.

## 2. Register the platform at your identity provider

At your identity provider, register an OpenID Connect client for the platform, with a client secret and the authorization code flow:

- the redirect address is `https://auth.vibe.example.com/callback`;
- the scopes are `openid profile email offline_access`. The provider must allow `offline_access` for this client and for every person who signs in;
- the ID token holds the person's Groups in the claim `groups`;
- the ID token is signed with `RS256` or `ES256`.

Note the issuer, the client id and the client secret. Choose the Group whose members are Platform administrators.

Then, on the server, print the setup credential. It is shown once, and it is good for 24 hours:

```sh
docker compose exec vibe vibe setup
```

## 3. Install the tool

On each computer that uses the tool: the administrator's for the next step, and each Builder's.

On Linux (`x86-64`):

```sh
curl -fLO https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0/vibe-0.1.0-x86_64-unknown-linux-gnu.tar.gz
curl -fLO https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0/SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf vibe-0.1.0-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin
cp vibe-0.1.0-x86_64-unknown-linux-gnu/vibe ~/.local/bin/vibe
```

On macOS (Apple silicon):

```sh
curl -fLO https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0/vibe-0.1.0-aarch64-apple-darwin.tar.gz
curl -fLO https://github.com/Irvingouj/vm-manager/releases/download/v0.1.0/SHA256SUMS
shasum -a 256 --check --ignore-missing SHA256SUMS
tar -xzf vibe-0.1.0-aarch64-apple-darwin.tar.gz
mkdir -p ~/.local/bin
cp vibe-0.1.0-aarch64-apple-darwin/vibe ~/.local/bin/vibe
```

The macOS program is not signed by a developer and not notarised by Apple. Downloaded with `curl`, as above, it runs. If you downloaded the archive with a browser, macOS holds the program back. Remove its quarantine mark:

```sh
xattr -d com.apple.quarantine ~/.local/bin/vibe
```

`~/.local/bin` must be on your `PATH`. If it is not, add this line to your shell's start file (`~/.bashrc` or `~/.zshrc`) and open a new terminal:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Check it:

```sh
vibe --version
```

The first line says `vibe 0.1.0`. Each archive also holds `THIRD-PARTY-LICENSES`: the licences of what the program is built from.

## 4. Connect OIDC

The administrator connects the identity provider with the tool, from any computer that reaches the installation over HTTPS. The two `read` lines ask for the setup credential and the client secret without showing them:

```sh
read -rs VIBE_SETUP_CREDENTIAL && export VIBE_SETUP_CREDENTIAL
read -rs VIBE_OIDC_CLIENT_SECRET && export VIBE_OIDC_CLIENT_SECRET
vibe admin connect-oidc --domain vibe.example.com \
    --issuer https://idp.example.com \
    --client-id vibe \
    --administrators-group vibe-administrators
```

Now `curl https://api.vibe.example.com/v1/installation` says `"sign_in":"oidc"`.

## 5. Sign in

The Builder signs the tool in. It opens the browser at the company's sign-in, and comes back signed in:

```sh
vibe sign-in --domain vibe.example.com
vibe whoami
```

On a computer without a browser, add `--no-browser`: the tool shows an address to open on another device.

## 6. Make a project

Make a project from the template, install its packages, and check it:

```sh
vibe new hello
cd hello
npm install
vibe check
```

`hello` is the Application's name. It becomes its address: `https://hello.vibe.example.com/`. The project's `AGENTS.md` tells you and your coding agent how to work in it.

## 7. Run it on your computer

```sh
vibe dev
```

Open `http://hello.localhost:7480/` in your browser. You choose there which test User you are; the test Users are in `vibe.dev.json`. The project is built again each time you change it. Stop it with `Ctrl+C`.

The first run downloads Postgres 18 once, for the Application's own database on your computer. You can also send one request without a browser:

```sh
vibe request --as outsider /
```

## 8. Deploy it

A deploy sends the project's last commit, so the project must be a Git repository with everything committed:

```sh
git init
git add --all
git commit -m "The first version"
vibe deploy
```

The Application now answers at `https://hello.vibe.example.com/`. Open it in your browser: you are its Contributor, so you can open it.

## 9. Share it

Let others open it. Share it with a person by their email address (they must have signed in to the installation once), with a Group, or with everyone who can sign in:

```sh
vibe share hello --email colleague@example.com
vibe share hello --group engineering
vibe share hello --everyone
```

## What next

- `vibe --help` lists every command, and `vibe <command> --help` says what each one does.
- The Dashboard is at `https://dashboard.vibe.example.com/`.
- What this release does not have yet is in the [release's notes](releases/v0.1.0.md).
