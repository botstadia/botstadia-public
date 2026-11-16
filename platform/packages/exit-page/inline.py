"""Assemble the exit page: one self-contained HTML file (TRUST.md#a-way-out-without-us).

The page's wasm goes in as base64 and its wasm-bindgen glue inline, so the
file needs nothing but itself and the public endpoints it talks to.
Usage: inline.py <index.html> <glue.js> <module.wasm> <out.html>
"""
import base64
import sys

template, glue, wasm, out = sys.argv[1:5]
html = open(template, encoding="utf-8").read()
js = open(glue, encoding="utf-8").read()
b64 = base64.b64encode(open(wasm, "rb").read()).decode("ascii")
assert "/*__GLUE__*/" in html and '"__WASM_BASE64__"' in html
html = html.replace("/*__GLUE__*/", js, 1).replace('"__WASM_BASE64__"', '"' + b64 + '"', 1)
open(out, "w", encoding="utf-8").write(html)
print(f"{out}: {len(html) // 1024} KiB")
