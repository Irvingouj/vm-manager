# TLS

The platform is served over HTTPS only, with a certificate for `*.vibe.example.com`. There are three ways to end TLS:

- **The Compose file's reverse proxy**, as `compose.yaml` starts it: put the certificate in `certs/` ([3. The certificate](admin/install/#3-the-certificate)) and start with `docker compose up -d`.
- **Your own reverse proxy**, with `compose.own-proxy.yaml`: below.
- **The program itself**, with `compose.own-tls.yaml`: below.

