#!/usr/bin/env python3
"""Run benchmarks for all 4 harnesses in parallel."""

import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from datetime import datetime

BENCH_DIR = Path(__file__).parent.parent
DATASETS_DIR = BENCH_DIR / "datasets"
RESULTS_DIR = BENCH_DIR / "results"
NEXUS_ENDPOINT = os.environ.get("NEXUS_ENDPOINT", "http://127.0.0.1:30001/v1")
NODE_PATH = "/Users/sonln4/.nvm/versions/node/v22.22.0/bin"

BENCHMARKS = {
    "aime_2026": {"cat": "reasoning", "target": 99.2},
    "gpqa_diamond": {"cat": "reasoning", "target": 91.2},
    "hle": {"cat": "reasoning", "target": 40.5},
    "hmmt_nov_2025": {"cat": "reasoning", "target": 94.4},
    "hmmt_feb_2026": {"cat": "reasoning", "target": 92.5},
    "imo_answerbench": {"cat": "reasoning", "target": 91.0},
    "critpt": {"cat": "reasoning", "target": 20.9},
    "terminal_bench": {"cat": "coding", "target": 81.0},
    "swe_bench_pro": {"cat": "coding", "target": 62.1},
    "mcp_atlas": {"cat": "agentic", "target": 76.8},
    "tool_decathlon": {"cat": "agentic", "target": 48.2},
}

HARNESS_CMDS = {
    "claude-cli": lambda p: (["claude", "-p", "--output-format", "text", p], os.environ, 180),
    "codex-cli": lambda p: ([f"{NODE_PATH}/codex", "exec", p], 
                           {**os.environ, "PATH": f"{NODE_PATH}:{os.environ.get('PATH','')}"}, 180),
    "codex-app": lambda p: (["/Applications/Codex.app/Contents/Resources/codex", "exec", p], os.environ, 180),
    "opencode": lambda p: ([f"{NODE_PATH}/opencode", "run", "--model", "nexus/glm-5.2", p],
                          {**os.environ, "PATH": f"{NODE_PATH}:{os.environ.get('PATH','')}"}, 180),
}


def extract_answer(text):
    boxed = re.findall(r'\\boxed\{([^}]+)\}', text)
    if boxed:
        return boxed[-1].strip()
    pat = re.search(r'(?:answer is|Answer:|answer:)\s*([^\n.]+)', text, re.IGNORECASE)
    if pat:
        return pat.group(1).strip().rstrip('.')
    lines = [l.strip() for l in text.strip().split('\n') if l.strip()]
    return lines[-1] if lines else text.strip()


def norm(s):
    s = str(s).strip().lower()
    s = re.sub(r'[\s{}\\]', '', s)
    s = s.replace('*', '').replace('^', '')
    s = re.sub(r'\\text\{[^}]*\}', '', s)
    return s


def score(response, expected, q, scoring_type):
    if not response:
        return False
    if scoring_type == "exact":
        ans = extract_answer(response)
        na, ne = norm(ans), norm(expected)
        if na == ne:
            return True
        nums_a = re.findall(r'\d+\.?\d*', ans)
        nums_e = re.findall(r'\d+\.?\d*', str(expected))
        if nums_a and nums_e and nums_a[-1] == nums_e[-1]:
            return True
        return ne in na or na in ne
    elif scoring_type == "choice":
        ans = extract_answer(response).lower()
        exp = expected.lower()
        if exp in ans:
            return True
        for c in q.get("choices", []):
            if c.lower() != exp and c.lower() in ans:
                return False
        return exp in ans
    elif scoring_type == "semantic":
        rl = response.lower()
        words = re.findall(r'\b\w+\b', expected.lower())
        key = [w for w in words if len(w) > 2]
        hits = sum(1 for w in key if w in rl)
        return hits >= max(1, len(key) * 0.4)
    elif scoring_type == "code":
        rl = response.lower()
        has_code = '```' in response or 'def ' in response or 'import' in rl
        words = re.findall(r'\b\w+\b', expected.lower())
        key = [w for w in words if len(w) > 3]
        hits = sum(1 for w in key if w in rl)
        return has_code and hits >= max(1, len(key) * 0.3)
    elif scoring_type == "task":
        rl = response.lower()
        words = re.findall(r'\b\w+\b', expected.lower())
        key = [w for w in words if len(w) > 3]
        hits = sum(1 for w in key if w in rl)
        return hits >= max(1, len(key) * 0.3)
    return False


def get_scoring_type(bench_key):
    cat = BENCHMARKS[bench_key]["cat"]
    if cat == "reasoning":
        if bench_key in ("gpqa_diamond",):
            return "choice"
        if bench_key in ("hle", "critpt"):
            return "semantic"
        return "exact"
    elif cat == "coding":
        return "code"
    else:
        return "task"


def run_single(harness_key, bench_key, questions, limit=3):
    cmd_fn = HARNESS_CMDS[harness_key]
    bench = BENCHMARKS[bench_key]
    stype = get_scoring_type(bench_key)
    qs = questions[:limit]
    results = []
    correct = 0
    
    for i, q in enumerate(qs):
        prompt = q.get("question", q.get("task", ""))
        if bench["cat"] == "reasoning":
            prompt += "\n\nSolve step by step. Put your final answer in \\boxed{}."
        elif bench["cat"] == "coding":
            prompt += "\n\nProvide a working code solution with explanation."
        
        cmd, env, timeout = cmd_fn(prompt)
        try:
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout, env=env)
            output = r.stdout
            ok = r.returncode == 0
        except subprocess.TimeoutExpired:
            output = ""
            ok = False
        except Exception:
            output = ""
            ok = False
        
        expected = q.get("answer", q.get("expected", ""))
        is_ok = score(output, expected, q, stype) if ok else False
        
        if is_ok:
            correct += 1
        
        results.append({
            "id": q.get("id", f"q{i}"),
            "correct": is_ok,
            "extracted": extract_answer(output)[:80] if output else "TIMEOUT/ERR",
            "expected": str(expected)[:80],
        })
    
    sc = round(correct / len(qs) * 100, 1) if qs else 0
    return {
        "benchmark": bench_key,
        "harness": harness_key,
        "category": bench["cat"],
        "target": bench["target"],
        "score": sc,
        "correct": correct,
        "total": len(qs),
        "results": results,
    }


def main():
    limit = int(sys.argv[1]) if len(sys.argv) > 1 else 3
    harness_filter = sys.argv[2] if len(sys.argv) > 2 else None
    bench_filter = sys.argv[3] if len(sys.argv) > 3 else None
    
    RESULTS_DIR.mkdir(exist_ok=True)
    harnesses = [harness_filter] if harness_filter else list(HARNESS_CMDS.keys())
    benchmarks = [bench_filter] if bench_filter else list(BENCHMARKS.keys())
    
    all_results = []
    
    for hk in harnesses:
        print(f"\n{'#'*60}")
        print(f"# Harness: {hk}")
        print(f"{'#'*60}")
        
        for bk in benchmarks:
            bf = DATASETS_DIR / f"{bk}.json"
            if not bf.exists():
                continue
            with open(bf) as f:
                questions = json.load(f)
            
            print(f"\n  {bk} ({BENCHMARKS[bk]['cat']}, target={BENCHMARKS[bk]['target']})...")
            r = run_single(hk, bk, questions, limit)
            all_results.append(r)
            print(f"  => {r['score']}% ({r['correct']}/{r['total']})")
            
            # Save incremental
            with open(RESULTS_DIR / "parallel_results.json", "w") as f:
                json.dump({
                    "timestamp": datetime.now().isoformat(),
                    "model": "glm-5.2",
                    "endpoint": NEXUS_ENDPOINT,
                    "limit_per_benchmark": limit,
                    "results": all_results,
                }, f, indent=2)
    
    # Summary
    print(f"\n{'='*80}")
    print(f"{'Benchmark':<22} {'Category':<10} {'Harness':<14} {'Score':>7} {'Target':>7} {'Diff':>7}")
    print(f"{'-'*80}")
    for r in all_results:
        d = r["score"] - r["target"]
        print(f"{r['benchmark']:<22} {r['category']:<10} {r['harness']:<14} {r['score']:>6.1f}% {r['target']:>6.1f}% {d:>+6.1f}")
    print(f"\nResults: {RESULTS_DIR / 'parallel_results.json'}")


if __name__ == "__main__":
    main()
