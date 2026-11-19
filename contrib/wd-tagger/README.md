# WD Tagger companion

The companion runs in its own Python environment as a local HTTP service. It loads WD SwinV2 v3 and its label list at startup, then uses CPU ONNX Runtime for inference. Use Python 3.12 on macOS or Linux.

From this directory, install the dependencies and download the two model files from the same revision. The ONNX file is about 467 MB.

```sh
python3.12 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
mkdir -p models
model_revision=627aef95638667ddcaa3ac8ae625e88ea5b02f51
model_base="https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3/resolve/$model_revision"
curl -fL "$model_base/model.onnx" -o models/model.onnx
curl -fL "$model_base/selected_tags.csv" -o models/selected_tags.csv
```

Start it in a separate terminal. Stop it with `Ctrl-C`.

```sh
.venv/bin/python server.py --model-dir models
```

The default address is `127.0.0.1:8790`. If the port is in use, pass `--port` and set `[tag_suggestions] tagger_port` in `nujijushi` to the same value. Enable tag suggestions in the [app configuration](../../README.md#optional-artwork-tag-suggestions) and run `nujijushi doctor --offline` to check readiness. The service is intended for a single host and binds only to loopback. It processes image bytes in memory.

## Local HTTP protocol

- `GET /v1/health` returns `204` once the model and labels are loaded.
- `POST /v1/predict` accepts a single raw still image with `Content-Type: application/octet-stream`. The service accepts JPEG, PNG, WebP, GIF, BMP, and AVIF. The input limits are 20 MiB, 40 megapixels, and 8,192 pixels on either edge.
- A `200` response contains UTF-8 tab-separated rows with no header. Columns are the model's numeric Danbooru character tag ID, tag name, and score. Up to 20 character hits above `0.85` are ordered by descending score, then ID. A successful empty body means no match.
- The service returns `413` for an oversized body, `422` for an unsupported or unreadable image, and `503` when another image is being analyzed.
