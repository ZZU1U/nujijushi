# FoidFinder for tag suggestions

[FoidFinder](https://github.com/AlpinDale/foidfinder/tree/212f92a459115d055795ccddebcb88d4e56d43b2) runs as a separate process. Choose Haruka on CPU, Haruka or PixAI on Apple Silicon with MLX, or PixAI on NVIDIA with CUDA.

## Haruka on CPU

Use `uv` and a platform with an [ONNX Runtime CPU wheel](https://pypi.org/project/onnxruntime/1.32.0/#files). The setup uses uv-managed Python 3.12. The launcher uses FoidFinder's server and tag ranking with the official [Haruka ONNX model](https://huggingface.co/KaraKaraWitch/haruka-tagger/tree/a65f3de8a32da3a40eec6b41bd45b47ad1a1e7be). It needs no GPU runtime. Run these commands from `contrib/foidfinder` in the nujijushi checkout.

```sh
uv venv --managed-python --python 3.12
.venv/bin/python -c 'import ctypes'
uv pip install --python .venv/bin/python -r requirements-cpu.txt
.venv/bin/hf download KaraKaraWitch/haruka-tagger \
  haruka-tagger.onnx haruka-tagger_metadata.json \
  --revision a65f3de8a32da3a40eec6b41bd45b47ad1a1e7be --local-dir models/haruka-cpu
.venv/bin/python haruka_cpu.py --model-dir models/haruka-cpu
```

The download is about 835 MB. After setup the launcher loads the model from disk and can run offline. It listens on `127.0.0.1:8790` and uses up to four CPU threads by default. Use `--threads N` to adjust that or `--port N` to pick a different port. Stop it with Ctrl+C.

If `hf` reports `No module named '_ctypes'`, the Python installation behind `.venv` is missing a standard-library extension. Recreate the Haruka virtual environment with `uv venv --clear --managed-python --python 3.12`, then resume the setup at the `import ctypes` check. The model files live separately under `models/`.

## Apple Silicon GPU

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
