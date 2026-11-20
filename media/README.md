# The launch video and the demo video

Two videos of a release, made again for each release from the scripts here:

- `demo.mp4` (about 3 minutes): the release used for real, following `docs/getting-started.md`. Narration and scene list: [`demo-script.md`](demo-script.md).
- `launch.mp4` (about a minute): what Vibe Manager is and who it is for, cut from the demo's footage plus title cards. Narration and scene list: [`launch-script.md`](launch-script.md).

Nothing in the footage is mocked up. Terminal scenes are the real output of the commands shown, run against a real installation of the release, rendered as frames of a terminal. Browser scenes are screenshots of a real headless Firefox. Before anything is written to disk, the capture replaces each secret with `•` (the setup credential, the client secret, the passwords, the sign-in challenge), the machine's paths with `~`, and its host name with `ada-laptop` (the tool runs in a namespace whose host name is `ada-laptop`, because the sign-in page names the machine). The rendered files are attached to a release or the website. They are not committed: `media/out/` is ignored.

## What it needs

A Linux machine with Docker and Compose, Firefox 128 or later (the snap works: it may only use files under `~/snap/firefox/common/`), ffmpeg with libx264 and libass (`subtitles`), Python 3, Node 22, `openssl`, `unshare` (util-linux), the font Ubuntu (`fc-list | grep Ubuntu`), and the Infisical CLI with access to `DEEPINFRA_API_KEY`. Two free ports on 127.0.0.1: 18443 (the installation) and 18444 (the identity provider), and 2841 and 2842 for Marionette.

Everything is made under `$T74` (default `/tmp/t74`, mode 700).

## Steps

For a later release, first change the version in `capture/capture.py` (`REL`, the tool archive's name), in `rig/compose.t74.yaml` if the Compose file changed, and in the cards of `render.py`. Change the narration in the two scripts if what the release does changed: say only what it does (its notes, `docs/releases/v<version>.md`).

```sh
cd media
# 1. The rig's files: a certificate authority, a wildcard certificate for *.vibe.acme.test,
#    one for the identity provider, throwaway passwords, and a Keycloak realm with
#    ada@acme.test (Platform administrator and Builder) and grace@acme.test.
bash rig/mk.sh
# 2. The identity provider: Keycloak 26 as the container t74-idp on the network t74-net.
bash rig/idp-up.sh
# 3. The narration: one WAV per scene, DeepInfra's hexgrad/Kokoro-82M, voice af_heart.
#    Only the narration text is sent; the key is read inside curl's command substitution.
python3 tts.py                 # or: python3 tts.py d03 l02   (again, for the scenes named)
# 4. The footage: installs the release with its Compose file (project t74-vibe), connects
#    OIDC, signs in, vibe new / dev / deploy / share, and the browser screenshots.
python3 capture/capture.py     # every scene in order; later scenes need the earlier ones
# 5. The frames: terminal scenes, browser scenes in a window frame, the cards.
python3 render.py
# 6. The videos, captions and the poster: $T74/out/{demo,launch}.{mp4,srt}, poster.png
python3 assemble.py
# 7. Tear the rig down: the installation and its volume, Keycloak, the network, the browsers.
bash rig/down.sh
```

Then check the result: `ffprobe` each video (1920x1080, 30 fps, H.264 and AAC), extract a frame every ten seconds (`ffmpeg -i demo.mp4 -vf fps=1/10 f%03d.png`) and look at each for anything unreadable or secret, and check each narration clip is not silent (`ffmpeg -i <clip> -af volumedetect -f null -`).

## How the rig differs from a real installation

- There is no DNS and no port 443: Firefox resolves the test names to 127.0.0.1 (`network.dns.localDomains`), the tool is given `--port 18443 --resolve 127.0.0.1`, and `.env` holds `VIBE_HTTPS_PORT=18443`. A real installation leaves these out. The commands on screen show them, as they were run.
- The company's certificate authority is the rig's own: `.env` holds `VIBE_CA_FILE`, and the tool is given `VIBE_CA_FILE` (shown as an `export` in scene d05).
- Compose runs with `COMPOSE_PROJECT_NAME=t74-vibe` and `COMPOSE_FILE=compose.yaml:compose.t74.yaml` in its environment, so `docker compose up -d` on screen is the command as typed. The overlay `rig/compose.t74.yaml` only puts the platform on the network where the identity provider answers.
- Not shown: copying the certificate into `certs/` (the rig has no `sudo`, so the throwaway key is made readable to all instead of being given to the user 65532), and registering the client at Keycloak (the realm is imported by `rig/mk.sh`).
- The "server" (`admin@acme-server`) and "Ada's computer" (`ada@ada-laptop`) are one machine; the prompts name the roles.

## Files

- `demo-script.md`, `launch-script.md`: the narration and the scene list. `tts.py` and `assemble.py` read them: a scene is a `##` heading with its id and kind, a `Shows:` line, and its narration as `>` lines.
- `rig/`: `mk.sh` (the rig's files), `idp-up.sh` (Keycloak), `compose.t74.yaml` (the overlay), `down.sh` (tear-down).
- `capture/`: `ff.py` (a small Marionette client), `browser.py` (starts a headless Firefox with its own profile and trusts the rig's authority), `capture.py` (runs and records every scene).
- `tts.py`, `render.py`, `assemble.py`: the narration, the frames, the videos.
