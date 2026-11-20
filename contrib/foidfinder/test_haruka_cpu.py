import unittest
from pathlib import Path
from unittest.mock import patch

import numpy as np
from PIL import Image

from haruka_cpu import HarukaCpu, IMAGENET_MEAN, IMAGENET_STD, model_input


class HarukaCpuTest(unittest.TestCase):
    def test_preprocessing_letterboxes_and_composites_on_white(self):
        image = Image.new("RGBA", (4, 2), (0, 0, 0, 0))
        image.paste((255, 0, 0, 255), (0, 0, 2, 2))
        tensor = model_input(image)
        self.assertEqual(tensor.shape, (1, 3, 448, 448))
        self.assertEqual(tensor.dtype, np.float32)
        self.assertTrue(tensor.flags.c_contiguous)
        white = (1 - IMAGENET_MEAN) / IMAGENET_STD
        red = (np.array([1, 0, 0], np.float32) - IMAGENET_MEAN) / IMAGENET_STD
        np.testing.assert_array_equal(tensor[0, :, 1, 10], white)
        np.testing.assert_array_equal(tensor[0, :, 224, 10], red)
        np.testing.assert_array_equal(tensor[0, :, 224, 438], white)

    @patch("haruka_cpu.ort.InferenceSession")
    def test_uses_only_the_cpu_provider_and_returns_probabilities(self, session):
        backend = HarukaCpu(Path("haruka-tagger.onnx"), label_count=3, threads=2)
        self.assertEqual(session.call_args.kwargs["providers"], ["CPUExecutionProvider"])
        self.assertEqual(session.call_args.kwargs["sess_options"].intra_op_num_threads, 2)
        session.return_value.disable_fallback.assert_called_once()
        session.return_value.run.return_value = [np.array([[-1000, 0, 1000]], np.float32)]
        probabilities = backend.predict(Image.new("RGB", (448, 448)))
        np.testing.assert_array_equal(probabilities, [0.0, 0.5, 1.0])
        outputs, inputs = session.return_value.run.call_args.args
        self.assertEqual(outputs, ["logits"])
        self.assertEqual(inputs["images"].shape, (1, 3, 448, 448))


if __name__ == "__main__":
    unittest.main()
