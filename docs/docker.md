# Deploy with Docker

Install Docker Engine and the Compose plugin on your Linux server. Run these commands from a directory containing [compose.yaml](../compose.yaml). The image includes the admin UI, gallery-dl, and FFmpeg. For a private package, sign in to GitHub Container Registry before pulling. [Image tags and registry access](#choose-an-image)

```sh
sudo install -d -m 0700 -o 10001 -g 10001 data
docker compose pull
docker compose run --rm nujijushi init
docker compose run --rm nujijushi doctor --offline
docker compose up -d
```

Save the UI token printed by `init`. Edit `data/config.toml` with `sudoedit` to set `[telegram] bot_token` when you’re ready to connect Telegram and `[access] admins` if you want to use bot commands. Put site cookies in `data/cookies/`. After changing the configuration, run `docker compose restart nujijushi`.

The admin UI is published on the server’s loopback interface. From another machine, forward the port over SSH and open `http://127.0.0.1:8788` in your browser.

```sh
ssh -L 8788:127.0.0.1:8788 user@server
```

For Telegram webhooks, use a host reverse proxy. [Webhook proxy setup](operations.md#polling-or-webhook)

The default command runs the Telegram scheduler and the admin UI. To run the UI and background jobs on their own, set `command: ["serve"]` under `nujijushi` in `compose.yaml`. Use `docker compose logs -f nujijushi` to check startup.

## Choose an image

GitHub Actions publishes `ghcr.io/zzu1u/nujijushi:main` on pushes to `main`, and publishes versioned images for release tags. Each image supports Linux on x86-64 and ARM64. Compose defaults to `main`. Once the first stable tagged image is published, set `NUJIJUSHI_IMAGE=ghcr.io/zzu1u/nujijushi:latest` in a `.env` file beside `compose.yaml` to follow stable releases. Use a published `vX.Y.Z` tag in place of `latest` to pin an exact version.

GitHub creates new container packages as private. For private pulls, run `docker login ghcr.io` with a classic GitHub personal access token with `read:packages` and access to the package. The maintainer can set the package to Public in its settings for anonymous pulls. [GitHub Container Registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry)

To use a local checkout, build the image and select it for your Compose commands.

```sh
docker build -t nujijushi:local .
export NUJIJUSHI_IMAGE=nujijushi:local
```

Use the same setup steps for a local build, starting with `init` after creating `data/`.

## Back up and move the archive

The host's `data/` directory holds the SQLite database, artwork, configuration, and cookies. Keep a copy on another machine or disk so you can recover from a server failure. Stop the service before taking a full filesystem backup.

```sh
docker compose stop
(umask 077; sudo tar -C data -czf - . > "nujijushi-$(date +%Y%m%d-%H%M%S).tar.gz")
docker compose up -d
```

On a replacement server, use a fresh `data/` directory and restore the backup before starting the service. An existing native install can be moved the same way by copying its whole data directory into `data/` while it’s stopped. Keep one running instance per archive and update any host-specific paths in `data/config.toml` to their container equivalents.

```sh
sudo install -d -m 0700 -o 10001 -g 10001 data
sudo tar -xzf /path/to/nujijushi-backup.tar.gz -C data
sudo chown -R 10001:10001 data
docker compose pull
docker compose run --rm nujijushi doctor --offline
docker compose up -d
```

Back up `data/` before upgrading, select the image you want, then run `docker compose pull && docker compose up -d`. Database migrations run at startup.
