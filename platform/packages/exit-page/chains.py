"""The exit page's defaults per chain, from config.yml (TRUST.md#a-way-out-without-us).

Usage: chains.py <config.yml> <target>...   (testnet, prod)

Prints the JSON the page is built with (`EXIT_PAGE_CHAINS`): for each target,
its chain id's wallet contracts, the block our settlement contract's events
start at, and the public RPC and bundlers, tied to no account of ours.
Reads `KEY: value` entries and folded `>-` blocks, resolving `{{.KEY}}`.
"""
import json
import re
import sys


def config(path):
    lines = open(path, encoding="utf-8").read().splitlines()
    raw = {}
    for i, line in enumerate(lines):
        m = re.match(r"  ([A-Z0-9_]+):(.*)$", line)
        if not m:
            continue
        value = m.group(2).strip()
        if value in (">-", ">"):
            block = []
            for nxt in lines[i + 1 :]:
                if not nxt.startswith("    "):
                    break
                block.append(nxt.strip())
            value = " ".join(block)
        raw[m.group(1)] = value.strip('"')

    def resolve(value, depth=0):
        if depth > 8:
            sys.exit("config: templates nest too deep")
        return re.sub(r"\{\{\.([A-Z0-9_]+)\}\}", lambda m: resolve(raw[m.group(1)], depth + 1), value)

    return lambda key: resolve(raw[key])


def chain(c, target):
    t = target.upper()
    contracts = json.loads(c(f"WALLET_CONTRACTS_{t}"))
    return c(f"CHAIN_ID_{t}"), {
        "contracts": contracts,
        "start_block": int(c(f"CHAIN_{t}_START_BLOCK")),
        "rpc": c(f"WALLET_PUBLIC_RPC_{t}"),
        "bundlers": c(f"WALLET_PUBLIC_BUNDLERS_{t}").split(","),
    }


if __name__ == "__main__":
    path, *targets = sys.argv[1:]
    if not targets:
        sys.exit(__doc__)
    c = config(path)
    print(json.dumps(dict(chain(c, t) for t in targets), separators=(",", ":"), sort_keys=True))
