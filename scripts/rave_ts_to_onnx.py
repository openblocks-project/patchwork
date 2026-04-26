#!/usr/bin/env python3
"""
Convert a RAVE TorchScript (.ts) export to ONNX (.onnx).

The Patchwork RAVE node loads .onnx files via ort. ACIDS distributes RAVE
models as TorchScript (.ts) — even the "_onnx"-named ones, which refers to
the noiseless config that's *exportable* to ONNX, not the file format.

Usage:
    python scripts/rave_ts_to_onnx.py SRC.ts [DST.onnx] [N_LATENT]

Defaults:
    DST       = SRC with .ts replaced by .onnx
    N_LATENT  = 8

If you see a shape mismatch from model.decode, your model uses a different
latent dim — try 16 (or whatever the model's config says).
"""
import sys

import torch


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    src = sys.argv[1]
    dst = sys.argv[2] if len(sys.argv) > 2 else src.replace(".ts", ".onnx")
    n_latent = int(sys.argv[3]) if len(sys.argv) > 3 else 8

    from torch._export.converter import TS2EPConverter

    model = torch.jit.load(src, map_location="cpu").eval()

    # Wrap as a ScriptModule so the call to `model.decode` (a ScriptModule
    # method) is itself JIT-resolved. A plain nn.Module wrapper trips the
    # "submodule not part of active trace" error.
    class Decoder(torch.jit.ScriptModule):
        def __init__(self, m):
            super().__init__()
            self.m = m

        @torch.jit.script_method
        def forward(self, z):
            return self.m.decode(z)

    decoder = Decoder(model)
    z = torch.randn(1, n_latent, 1)

    # The legacy torch.onnx tracer can't handle RAVE's `cached_conv`
    # streaming buffers (runtime batch-shape slicing → ONNX::Gather rejects
    # `List[int]`). The dynamo exporter doesn't accept ScriptModules. Bridge
    # via TS2EPConverter: TorchScript → ExportedProgram → ONNX.
    ep = TS2EPConverter(decoder, (z,)).convert()
    torch.onnx.export(
        ep, (z,), dst,
        input_names=["z"], output_names=["audio"],
        optimize=False,        # the optimizer chokes on RAVE's cached_conv state
        external_data=False,   # bundle weights inline (RAVE models are small)
    )
    print(f"Wrote {dst}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
