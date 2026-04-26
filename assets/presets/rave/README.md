# RAVE Models

The **RAVE** node hosts ONNX-exported RAVE (Realtime Audio Variational autoEncoder)
models from IRCAM/ACIDS. Models are not bundled — download a `.onnx` file
from one of the sources below and load it via the node's file picker.

## Sources

- **Official model zoo**: <https://acids-ircam.github.io/rave_models_download>
- **HuggingFace** (community-trained):
  - <https://huggingface.co/Intelligent-Instruments-Lab/rave-models>
  - <https://huggingface.co/Tangible-Music-Lab/RAVE_models>

## Recommended starter model

Start with **`darbouka_onnx`** from the official zoo. It's the canonical
ONNX-exported RAVE model and is what this integration was first tested
against.

## Constraints

- ONNX format only (this branch). TorchScript `.ts` models from the broader
  RAVE zoo are not supported here — they require `tch-rs` / libtorch, which
  is a follow-up branch.
- The ONNX export must be the **noiseless** RAVE configuration (the variant
  RAVE provides a dedicated config for, since some operators in the full
  model don't trace cleanly through `torch.onnx.export`).
- The decoder is what's exposed: latent vectors → audio. Encode (audio →
  latents) is not implemented in this branch.

## Latent dimension

The node auto-detects the latent dimension from the model's input shape on
load and resizes the slider grid accordingly. Most published RAVE configs
use 8 latents; some use 16. Symbolic / dynamic dims fall back to 8.

## Quick test

1. Drop a **RAVE** node onto the canvas (Audio category, marked WIP).
2. Click **📂 Model…** and pick a `.onnx` file.
3. Wait for status to flip from *Loading…* to *Ready*.
4. Wire the node's Audio output into a **Speaker**.
5. Move the latent sliders. Audible neural-synth output should appear.

If you hear gaps, the model is too slow for real time on this host —
inspect Activity Monitor to see whether CoreML / CUDA is being used.
