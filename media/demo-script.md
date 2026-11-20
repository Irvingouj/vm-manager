# Demo video: narration and scene list

The demo shows release v0.1.0 used for real, following `docs/getting-started.md`, for a fictional company, Acme (`vibe.acme.test`), with two people: Ada (`ada@acme.test`, Platform administrator and Builder) and Grace (`grace@acme.test`, the person the Application is shared with). Every terminal scene is the real output of the command shown, captured by `capture/capture.py`; every browser scene is a screenshot of a real headless Firefox. Title cards are marked `card`.

Format, read by `tts.py` and `assemble.py`: each scene is a `##` heading with its id and kind (`card`, `term` or `web`), a `Shows:` line, and its narration as the `>` lines.

What is changed from what the commands printed: secrets (the setup credential) are replaced by `•` before a frame is rendered, and the rig's own paths are shown as `~/…`. The commands give `--port 18443 --resolve 127.0.0.1` because the rig has no DNS and no port 443; a real installation leaves them out.

## d01 card

Shows: title card "Vibe Manager 0.1, from install to a shared Application".

> This is Vibe Manager, version zero point one. In the next few minutes we install it for a fictional company, Acme, and take one small application from an empty folder to a colleague's browser.

## d02 term

Shows: `curl` downloading `compose.yaml`, `env.example` and `SHA256SUMS` from the release, and `sha256sum --check` saying OK.

> The administrator starts on a Linux server with Docker. They download the release's Compose file and its list of values, and check both against the published checksums.

## d03 term

Shows: `.env` made from `env.example` (domain, key, Postgres password), `docker compose up -d`, `docker compose ps`.

> They fill in three values: the domain, the installation's key, and a database password. Then docker compose starts three containers: the platform, its Postgres, and a proxy that serves the company's wildcard certificate.

## d04 term

Shows: `curl …/v1/installation` answering `"sign_in":"none"`, then `docker compose exec vibe vibe setup` printing the setup credential (masked with `•`).

> The installation answers, with no sign-in connected yet. The server then prints a setup credential, once. It is hidden here.

## d05 term

Shows: the tool downloaded from the release, checked, unpacked, and `vibe --version`.

> On a computer that reaches the installation, the administrator installs the tool, called vibe, from the same release.

## d06 term

Shows: the setup credential and the client secret read with `read -rs`, `vibe admin connect-oidc`, and `/v1/installation` now answering `"sign_in":"oidc"`.

> Acme's identity provider is Keycloak, where the platform is registered as a client. With the setup credential and the client secret typed in without showing them, the administrator connects it, and names the group whose members are platform administrators.

## d07 web

Shows: the Dashboard's address sends Ada to Acme's sign-in page (Keycloak); she signs in and lands on her Dashboard, empty.

> Ada, the administrator, also builds applications. She opens the Dashboard. The platform sends her to Acme's own sign-in page, and back to a Dashboard that is still empty.

## d08 term+web

Shows: `vibe sign-in --no-browser` printing an address and a check code; the browser's confirmation page with that code; the tool signed in; `vibe whoami`.

> Next she signs the tool in. It shows an address and a check code. She confirms the code in the browser, and the tool is signed in, as a platform administrator.

## d09 term

Shows: `vibe new hello`, `npm install`, `vibe check`.

> vibe new makes a project from the template, with an agents file that tells her coding agent how to work in it. vibe check says whether it will deploy.

## d10 term+web

Shows: `vibe dev` serving at `http://hello.localhost:7480/`; the page where she chooses a test User; the Application as that test User.

> vibe dev runs the application on her own computer, beside a private Postgres. In the browser she chooses which test user she is, and sees the page as that person.

## d11 term

Shows: `git init`, `git commit`, `vibe deploy`.

> A deploy sends the project's last commit. The tool builds it on her computer and sends it to the platform as a new version.

## d12 web

Shows: `https://hello.vibe.acme.test/` open for Ada; her Dashboard listing hello, with her as its Contributor.

> The application now answers at its own address, behind the company's sign-in. Ada's Dashboard lists it, with her as its contributor.

## d13 web

Shows: Grace signs in to the Dashboard (empty for her), then opens `https://hello.vibe.acme.test/` and is refused.

> Grace, a colleague, signs in too. When she opens the application's address, the platform refuses her: it has not been shared with her.

## d14 term

Shows: `vibe share hello --email grace@acme.test`.

> Ada shares it with Grace, by her email address. She could share it with a group, or with everyone who can sign in, in the same way.

## d15 web

Shows: Grace's Dashboard now listing hello as shared with her; the Application open for Grace.

> Now hello is on Grace's Dashboard, and it opens for her.

## d16 card

Shows: end card "Install. Sign in. Build. Run. Deploy. Share." with the release's limits and the repository's address.

> That is version zero point one: install, sign in, build, run locally, deploy and share. It is not yet for untrusted applications or real company data. The release notes say what is still missing, and getting started is in the repository.
