"""One-time conversion: openkeyscan3.pt -> spindex-key.onnx.

Bakes the CQT preprocessing (librosa-parity via nnAudio) into the ONNX graph
so the Rust sidecar only feeds raw 44.1 kHz mono PCM.

Usage:  py -3.12 scripts/convert-key-model.py   (set PYTHONUTF8=1 on Windows —
        torch's exporter prints emoji that crash a cp1252 console)
Needs:  pip install torch nnAudio librosa onnx onnxruntime onnxscript numpy soundfile
"""
from pathlib import Path

import numpy as np
import torch
import torch.nn as nn

MODELS = Path(__file__).resolve().parent.parent / "src-tauri" / "binaries" / "models"
CKPT = MODELS / "openkeyscan3.pt"
OUT = MODELS / "spindex-key.onnx"

SR, HOP, NBINS, BPO, FMIN = 44100, 8820, 105, 24, 65


# --- KeyNet, verbatim from rekordcloud/openkeyscan-analyzer model.py (MIT) ---
class BasicConv2d(nn.Module):
    def __init__(self, in_channels, out_channels, kernel_size):
        super().__init__()
        self.conv = nn.Conv2d(in_channels, out_channels, kernel_size,
                              padding="same", bias=False)
        self.bn = nn.BatchNorm2d(out_channels)
        self.elu = nn.ELU(inplace=True)

    def forward(self, x):
        return self.elu(self.bn(self.conv(x)))


class KeyNet(nn.Module):
    def __init__(self, num_classes=24, in_channels=1, Nf=20, p=0.5):
        super().__init__()
        self.conv1 = BasicConv2d(in_channels, Nf, 5)
        self.conv2 = BasicConv2d(Nf, Nf, 3)
        self.pool1 = nn.MaxPool2d(2)
        self.dropout1 = nn.Dropout2d(p=p)
        self.conv3 = BasicConv2d(Nf, 2 * Nf, 3)
        self.conv4 = BasicConv2d(2 * Nf, 2 * Nf, 3)
        self.pool2 = nn.MaxPool2d(2)
        self.dropout2 = nn.Dropout2d(p=p)
        self.conv5 = BasicConv2d(2 * Nf, 4 * Nf, 3)
        self.conv6 = BasicConv2d(4 * Nf, 4 * Nf, 3)
        self.pool3 = nn.MaxPool2d(2)
        self.dropout3 = nn.Dropout2d(p=p)
        self.conv7 = BasicConv2d(4 * Nf, 8 * Nf, 3)
        self.dropout4 = nn.Dropout2d(p=p)
        self.conv8 = BasicConv2d(8 * Nf, 8 * Nf, 3)
        self.dropout5 = nn.Dropout2d(p=p)
        self.conv9 = BasicConv2d(8 * Nf, num_classes, 1)
        self.global_avgpool = nn.AdaptiveAvgPool2d((1, 1))

    def forward(self, x):
        x = self.dropout1(self.pool1(self.conv2(self.conv1(x))))
        x = self.dropout2(self.pool2(self.conv4(self.conv3(x))))
        x = self.dropout3(self.pool3(self.conv6(self.conv5(x))))
        x = self.dropout4(self.conv7(x))
        x = self.dropout5(self.conv8(x))
        x = self.conv9(x)
        return torch.flatten(self.global_avgpool(x), 1)
# --- end KeyNet ---


class KeyPipeline(nn.Module):
    """CQT (nnAudio, librosa-parity) + log1p + KeyNet, PCM in, logits out."""

    def __init__(self, net: KeyNet, bin_scale: torch.Tensor):
        super().__init__()
        from nnAudio.features.cqt import CQT1992v2
        self.cqt = CQT1992v2(sr=SR, hop_length=HOP, fmin=FMIN, n_bins=NBINS,
                             bins_per_octave=BPO, output_format="Magnitude",
                             verbose=False)
        self.register_buffer("bin_scale", bin_scale.reshape(1, NBINS, 1))
        self.net = net

    def forward(self, pcm):                 # (1, N) float32, 44.1 kHz mono
        spec = self.cqt(pcm)                # (1, 105, T)
        spec = spec * self.bin_scale        # align nnAudio scaling to librosa
        spec = torch.log1p(spec)
        return self.net(spec.unsqueeze(1))  # (1, 24)


def librosa_spec(pcm: np.ndarray) -> np.ndarray:
    import librosa
    c = librosa.cqt(pcm, sr=SR, hop_length=HOP, n_bins=NBINS,
                    bins_per_octave=BPO, fmin=FMIN)
    return np.abs(c).astype(np.float32)     # (105, T)


def fit_bin_scale(net_cqt) -> torch.Tensor:
    """Per-bin scale aligning nnAudio magnitudes with librosa on white noise."""
    rng = np.random.default_rng(42)
    noise = rng.standard_normal(SR * 10).astype(np.float32) * 0.3
    ref = librosa_spec(noise)                # (105, T)
    with torch.no_grad():
        got = net_cqt(torch.from_numpy(noise[None]))[0].numpy()  # (105, T)
    t = min(ref.shape[1], got.shape[1])
    ratio = ref[:, 1:t - 1].mean(axis=1) / np.maximum(got[:, 1:t - 1].mean(axis=1), 1e-9)
    print(f"bin_scale: min {ratio.min():.4f} max {ratio.max():.4f} "
          f"mean {ratio.mean():.4f}")
    return torch.from_numpy(ratio.astype(np.float32))


def load_keynet() -> KeyNet:
    # openkeyscan3.pt is a wider variant than model.py's demo default: Nf=64
    # (checkpoint conv1 weight shape [64, 1, 5, 5]).
    net = KeyNet(Nf=64)
    try:
        # Safe path: plain tensors only, no arbitrary unpickling.
        obj = torch.load(CKPT, map_location="cpu", weights_only=True)
    except Exception as e:
        # The checkpoint pickles full Python objects. Unpickling executes
        # arbitrary code from the file — we accept that ONLY because this is
        # the official MIT-licensed checkpoint fetched from the upstream repo.
        print(f"weights_only load failed ({e}); falling back to full unpickle "
              f"of the trusted upstream checkpoint")
        obj = torch.load(CKPT, map_location="cpu", weights_only=False)
    if isinstance(obj, dict) and not hasattr(obj, "forward"):
        state = obj.get("model_state_dict") or obj.get("state_dict") or obj
        net.load_state_dict(state)
    else:  # a whole pickled module
        net.load_state_dict(obj.state_dict())
    net.eval()
    return net


def parity_check(onnx_path: Path, net: KeyNet):
    """Full-pipeline check: librosa preprocessing + torch net vs exported ONNX."""
    import onnxruntime as rt
    rng = np.random.default_rng(7)
    # 20 s pseudo-music: A minor chord + noise floor
    t = np.arange(SR * 20) / SR
    pcm = sum(np.sin(2 * np.pi * f * t) * a for f, a in
              [(220.0, .5), (261.63, .4), (329.63, .4), (440.0, .2)])
    pcm = (pcm + rng.standard_normal(len(t)) * 0.02).astype(np.float32)

    ref_spec = np.log1p(librosa_spec(pcm))
    with torch.no_grad():
        ref_logits = net(torch.from_numpy(ref_spec[None, None])).numpy()[0]

    sess = rt.InferenceSession(str(onnx_path), providers=["CPUExecutionProvider"])
    got_logits = sess.run(["logits"], {"pcm": pcm[None]})[0][0]

    print(f"reference argmax {ref_logits.argmax()}  onnx argmax {got_logits.argmax()}")
    print(f"max logit diff {np.abs(ref_logits - got_logits).max():.4f}")
    assert ref_logits.argmax() == got_logits.argmax(), "PARITY FAILURE"
    print("parity OK")


def main():
    net = load_keynet()
    from nnAudio.features.cqt import CQT1992v2
    probe = CQT1992v2(sr=SR, hop_length=HOP, fmin=FMIN, n_bins=NBINS,
                      bins_per_octave=BPO, output_format="Magnitude",
                      verbose=False)
    scale = fit_bin_scale(probe)
    pipe = KeyPipeline(net, scale).eval()

    dummy = torch.zeros(1, SR * 5)
    torch.onnx.export(
        pipe, dummy, str(OUT),
        input_names=["pcm"], output_names=["logits"],
        dynamic_axes={"pcm": {1: "samples"}},
        opset_version=17,
    )
    print(f"wrote {OUT}")
    parity_check(OUT, net)


if __name__ == "__main__":
    main()
