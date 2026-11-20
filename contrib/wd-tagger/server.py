"""Loopback-only WD SwinV2 tagger for nujijushi's v1 prediction protocol."""

import argparse
import asyncio
import csv
from io import BytesIO
import os
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image, ImageOps, UnidentifiedImageError
from starlette.applications import Starlette
from starlette.concurrency import run_in_threadpool
from starlette.middleware import Middleware
from starlette.middleware.trustedhost import TrustedHostMiddleware
from starlette.requests import ClientDisconnect
from starlette.responses import Response
from starlette.routing import Route
import uvicorn

MAX_IMAGE_BYTES = 20 * 1024 * 1024
MAX_PIXELS = 40_000_000
MAX_EDGE = 8192
IMAGE_FORMATS = ("JPEG", "PNG", "WEBP", "GIF", "BMP", "AVIF")


class UnsupportedImage(ValueError):
    pass


def image_tensor(data, size):
    with Image.open(BytesIO(data), formats=IMAGE_FORMATS) as original:
        if getattr(original, "is_animated", False):
            raise UnsupportedImage("animated image")
        if max(original.size) > MAX_EDGE or original.width * original.height > MAX_PIXELS:
            raise UnsupportedImage("image dimensions exceed the limit")
        rgba = ImageOps.exif_transpose(original).convert("RGBA")
    edge = max(rgba.size)
    square = Image.new("RGB", (edge, edge), "white")
    offset = ((edge - rgba.width) // 2, (edge - rgba.height) // 2)
    square.paste(rgba, offset, rgba.getchannel("A"))
    square = square.resize((size, size), Image.Resampling.BICUBIC)
    # The ONNX export takes unscaled BGR float32 in NHWC order.
    pixels = np.asarray(square, dtype=np.float32)
    return np.ascontiguousarray(pixels[None, :, :, ::-1])


class Tagger:
    def __init__(self, model_dir):
        with (model_dir / "selected_tags.csv").open(newline="", encoding="utf-8") as labels:
            rows = list(csv.DictReader(labels))
        self.characters = [
            (index, int(row["tag_id"]), row["name"])
            for index, row in enumerate(rows)
            if row["category"] == "4"
        ]
        if not self.characters:
            raise ValueError("selected_tags.csv has no character labels")
        options = ort.SessionOptions()
        options.intra_op_num_threads = min(4, os.cpu_count() or 1)
        self.session = ort.InferenceSession(
            model_dir / "model.onnx", sess_options=options, providers=["CPUExecutionProvider"]
        )
        model_input = self.session.get_inputs()[0]
        _, height, width, channels = model_input.shape
        output = self.session.get_outputs()[0]
        if not isinstance(height, int) or height <= 0 or height != width or channels != 3:
            raise ValueError("unexpected WD Tagger input shape")
        if output.shape[-1] != len(rows):
            raise ValueError("model output does not match selected_tags.csv")
        self.size = height
        self.input_name = model_input.name
        self.output_name = output.name

    def predict(self, data):
        try:
            tensor = image_tensor(data, self.size)
        except (Image.DecompressionBombError, UnidentifiedImageError, OSError, ValueError) as err:
            raise UnsupportedImage("invalid or unsupported still image") from err
        scores = self.session.run([self.output_name], {self.input_name: tensor})[0][0]
        hits = [
            (tag_id, name, float(scores[index]))
            for index, tag_id, name in self.characters
            if 0.0 <= scores[index] <= 1.0
        ]
        hits.sort(key=lambda row: (-row[2], row[0]))
        return "".join(f"{tag_id}\t{name}\t{score}\n" for tag_id, name, score in hits[:20])


def app_for(tagger):
    slot = asyncio.Lock()

    async def health(_request):
        return Response(status_code=204)

    async def predict(request):
        if request.headers.get("content-type") != "application/octet-stream":
            return Response(status_code=415)
        if slot.locked():
            return Response(status_code=503)
        async with slot:
            data = bytearray()
            try:
                async with asyncio.timeout(5):
                    async for chunk in request.stream():
                        if len(data) + len(chunk) > MAX_IMAGE_BYTES:
                            return Response(status_code=413)
                        data.extend(chunk)
            except (TimeoutError, ClientDisconnect):
                return Response(status_code=408)
            try:
                rows = await run_in_threadpool(tagger.predict, data)
            except UnsupportedImage:
                return Response(status_code=422)
            return Response(rows, media_type="text/tab-separated-values")

    return Starlette(
        routes=[Route("/v1/health", health), Route("/v1/predict", predict, methods=["POST"])],
        middleware=[Middleware(TrustedHostMiddleware, allowed_hosts=["127.0.0.1"])],
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-dir", type=Path, required=True)
    parser.add_argument("--port", type=int, default=8790)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("--port must be between 1 and 65535")
    app = app_for(Tagger(args.model_dir))
    uvicorn.run(app, host="127.0.0.1", port=args.port, proxy_headers=False, access_log=False)


if __name__ == "__main__":
    main()
