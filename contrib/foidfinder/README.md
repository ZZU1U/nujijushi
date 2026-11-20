# FoidFinder for tag suggestions

[FoidFinder](https://github.com/AlpinDale/foidfinder/tree/212f92a459115d055795ccddebcb88d4e56d43b2) runs as a separate process. On Apple Silicon it can run Haruka or PixAI through MLX. The NVIDIA service runs PixAI through CUDA. Tag suggestions are optional, and the upstream HTTP server requires one of those GPU backends.

## Apple Silicon

Install Python 3.12 or newer and `uv`. Clone FoidFinder into its own directory, then install its Apple Silicon dependencies and prepare Haruka.

```sh
git clone https://github.com/AlpinDale/foidfinder.git
cd foidfinder
git checkout 212f92a459115d055795ccddebcb88d4e56d43b2
uv sync --locked --no-dev --extra mlx
uv pip install safetensors
uv run --no-sync foidfinder convert-haruka models/haruka
uv run --no-sync foidfinder --model-dir models/haruka serve --host 127.0.0.1 --port 8790
```

To use PixAI on the same machine, run `uv run --no-sync foidfinder download` and then `uv run --no-sync foidfinder serve --host 127.0.0.1 --port 8790`. The download or conversion runs once. Start the server when you want tag suggestions and stop it with Ctrl+C.

## NVIDIA

Follow the [upstream Docker instructions](https://github.com/AlpinDale/foidfinder/tree/212f92a459115d055795ccddebcb88d4e56d43b2#docker-nvidia) for your GPU. The compose configuration publishes to `127.0.0.1:8000`. Set `tagger_port = 8000` in nujijushi when using that setup.

## Connect nujijushi

Enable tag suggestions in [nujijushi's configuration](../../README.md#optional-artwork-tag-suggestions). If you used the old WD Tagger service, stop it to free port 8790. Existing tag reviews and numeric franchise mappings carry over.

Run `nujijushi doctor --offline` to check the service. The archive calls `GET /api/health` and sends still images directly to `POST /api/tag` on loopback. It considers up to 12 images per post, with a 20 MiB limit per image. FoidFinder's model thresholds determine which tags count as strong matches. The app uses character and series tags and links each model suggestion to its supporting images.

FoidFinder returns character names. The archive recognizes the local starter names and remembers names resolved through the optional Danbooru fallback. To use an existing numeric mapping offline for a name the archive has never seen, bind the exact name shown in the review with `nujijushi tagger map <character-id> <franchise> --name <foidfinder-label>`.
