"""Serve Haruka's official ONNX model through FoidFinder with CPU inference."""

import argparse
import os
from pathlib import Path

import numpy as np
import onnxruntime as ort
import uvicorn
from foidfinder.haruka import IMAGENET_MEAN, IMAGENET_STD, METADATA, MODEL, THRESHOLDS, vocab
from foidfinder.image import rgb_on_white
from foidfinder.tagging import TagIndex
from foidfinder.web import create_app
from PIL import Image


IMAGE_SIZE = MODEL["img_size"]
WEIGHTS = "haruka-tagger.onnx"
CPU_PROVIDER = "CPUExecutionProvider"


def model_input(image: Image.Image) -> np.ndarray:
    """Haruka's white-padded bicubic resize and ImageNet normalization."""
    image = rgb_on_white(image)
    width, height = image.size
    if (width, height) != (IMAGE_SIZE, IMAGE_SIZE):
        scale = IMAGE_SIZE / max(width, height)
        resized = (max(1, int(width * scale)), max(1, int(height * scale)))
        panel = image.resize(resized, Image.Resampling.BICUBIC)
        image = Image.new("RGB", (IMAGE_SIZE, IMAGE_SIZE), "white")
        image.paste(panel, ((IMAGE_SIZE - resized[0]) // 2, (IMAGE_SIZE - resized[1]) // 2))
    pixels = np.asarray(image, dtype=np.float32) / 255.0
    normalized = (pixels - IMAGENET_MEAN) / IMAGENET_STD
    return np.ascontiguousarray(normalized.transpose(2, 0, 1)[None])


class HarukaCpu:
    name = "onnx-cpu"
    device = "CPU"

    def __init__(self, weights: Path, label_count: int, threads: int):
        options = ort.SessionOptions()
        options.intra_op_num_threads = threads
        options.add_session_config_entry("session.intra_op.allow_spinning", "0")
        self.session = ort.InferenceSession(
            str(weights), sess_options=options, providers=[CPU_PROVIDER]
        )
        self.session.disable_fallback()
        self.label_count = label_count

    def predict(self, image: Image.Image) -> np.ndarray:
        (logits,) = self.session.run(["logits"], {"images": model_input(image)})
        if logits.shape != (1, self.label_count) or not np.isfinite(logits).all():
            raise ValueError("Haruka model output does not match its tag metadata")
        logits = logits[0]
        magnitude = np.exp(-np.abs(logits))
        return np.where(logits >= 0, 1.0, magnitude) / (1.0 + magnitude)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model-dir", required=True, type=Path)
    parser.add_argument("--port", default=8790, type=int)
    parser.add_argument("--threads", default=min(4, os.cpu_count() or 1), type=int)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("--port must be between 1 and 65535")
    if args.threads < 1:
        parser.error("--threads must be at least 1")

    weights = args.model_dir / WEIGHTS
    metadata = args.model_dir / METADATA
    for path in (weights, metadata):
        if not path.is_file():
            parser.error(f"missing {path}. Download the model files first.")
    tags, categories = vocab(metadata)
    index = TagIndex({"model": MODEL, "tags": tags, "categories": categories, "thresholds": THRESHOLDS})
    app = create_app(lambda: HarukaCpu(weights, len(tags), args.threads), index)
    uvicorn.run(app, host="127.0.0.1", port=args.port, proxy_headers=False, server_header=False)


if __name__ == "__main__":
    main()
