# One-off model preparation so tract can load the vendored ONNX graphs.
#   uv run --with onnx --with onnxsim tools/prep_models.py
import onnx
from onnxsim import simplify

# Blendshapes: constant-fold the shape arithmetic feeding ReduceMean's axes.
m = onnx.load("models/face_blendshapes.onnx")
m, ok = simplify(m, overwrite_input_shapes={"serving_default_input_points:0": [1, 146, 2]})
assert ok
onnx.save(m, "models/face_blendshapes.sim.onnx")

# YuNet: drop the baked 640x640 shapes so any input size (multiple of 32) works.
m = onnx.load("models/face_detection_yunet_2023mar.onnx")
del m.graph.value_info[:]
for o in m.graph.output:
    o.type.tensor_type.ClearField("shape")
d = m.graph.input[0].type.tensor_type.shape.dim
d[2].dim_param, d[3].dim_param = "h", "w"
for n in m.graph.node:
    if n.op_type == "Reshape":
        print("reshape", n.name, n.input)
onnx.save(m, "models/face_detection_yunet.dyn.onnx")
