# Host fixtures

What the session host's tests run, built by the tests themselves (`tests/session.rs`):

- `arena/`: a tiny arena. Each seat's input is the turn number, each answers one value in [-1, 1], and after twenty turns the higher sum wins.
- `bot/`: one bot crate with a behaviour per feature (`steady`, `spiky`, `forever`, `trap`, `memcopy`, `sleep`, `clock`, `budget`, `declaration`, `infer`), each built separately.
- `scale.onnx`: `y = x · 0.01`, the model the `infer` bot calls.
- `policy.onnx`: a trained policy for the fixture arena by the shell's contract: `value = tanh(turn · 0.1)`.
- `memory.onnx`: the same with memory: `state_out = state_in + 1`, `value = tanh(sum(state_out) · 0.05)`.

The models were made with the `onnx` Python package's helpers.
