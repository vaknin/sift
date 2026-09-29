# Models

Vendored ONNX models, compiled into `sift-engine` with `include_bytes!` and run by tract.

| File | Used | What | Source | License |
|---|---|---|---|---|
| `face_detection_yunet_2023mar.onnx` | no (original) | YuNet face detector, fixed 640×640 input | [opencv_zoo `models/face_detection_yunet`](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet) | MIT, © 2020 Shiqi Yu |
| `face_detection_yunet.dyn.onnx` | yes | YuNet with dynamic input size (generated) | `tools/prep_models.py` | MIT |
| `face_landmarks.onnx` | yes | MediaPipe Face Mesh V2: 478 landmarks, input `[1,256,256,3]` RGB 0–1 | [`fernandotonon/QtMeshEditor-models` `mocap/face/`](https://huggingface.co/fernandotonon/QtMeshEditor-models), converted from Google MediaPipe Face Landmarker | Apache-2.0 |
| `face_blendshapes.onnx` | no (original) | MediaPipe blendshapes: 52 scores from 146 landmarks `[1,146,2]` in image pixels | same | Apache-2.0 |
| `face_blendshapes.sim.onnx` | yes | Blendshapes with constant-folded shapes (generated) | `tools/prep_models.py` | Apache-2.0 |
| `face_recognition_sface_2021dec.onnx` | yes | SFace identity embedding: input `[1,3,112,112]` RGB 0–255, face aligned from YuNet's 5 landmarks; output 128-d | [opencv_zoo `models/face_recognition_sface`](https://github.com/opencv/opencv_zoo/tree/main/models/face_recognition_sface) (also on Hugging Face `opencv/face_recognition_sface`) | Apache-2.0 |

The generated files come from the originals via:

    uv run --with onnx --with onnxsim python tools/prep_models.py

SHA-256 of the originals as downloaded:

    8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4  face_detection_yunet_2023mar.onnx
    d16e5a55e6a480284d468ee32469692464049518c311662ab3956681de31e3e9  face_landmarks.onnx
    37cf54d3ccf671f8936427437a1a0b627799e16f20439de3cc35aa189ad40c66  face_blendshapes.onnx
    0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79  face_recognition_sface_2021dec.onnx

The MediaPipe conversions are by QtMeshEditor (`scripts/export-facecap-onnx.py`,
numerical parity checked against the Python mediapipe reference). Apache-2.0:
https://www.apache.org/licenses/LICENSE-2.0
