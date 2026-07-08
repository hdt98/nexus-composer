#!/usr/bin/env python3
"""GLM 5.2 Benchmark Runner - evaluates each harness across all benchmark datasets."""

import json
import os
import re
import subprocess
import time
import sys
from pathlib import Path
from datetime import datetime

BENCH_DIR = Path(__file__).parent.parent
DATASETS_DIR = BENCH_DIR / "datasets"
RESULTS_DIR = BENCH_DIR / "results"

NODE_PATH = "/Users/sonln4/.nvm/versions/node/v22.22.0/bin"

HARNESSES = {
    "claude-cli": {
        "name": "Claude Code CLI",
        "command": lambda prompt: ["claude", "-p", "--output-format", "text", prompt],
        "timeout": 90,
    },
    "codex-cli": {
        "name": "Codex CLI",
        "command": lambda prompt: [f"{NODE_PATH}/codex", "exec", prompt],
        "timeout": 90,
        "env": {**os.environ, "PATH": f"{NODE_PATH}:" + os.environ.get("PATH", "")},
    },
    "codex-app": {
        "name": "Codex App",
        "command": lambda prompt: ["/Applications/Codex.app/Contents/Resources/codex", "exec", prompt],
        "timeout": 90,
    },
    "opencode": {
        "name": "OpenCode",
        "command": lambda prompt: [f"{NODE_PATH}/opencode", "run", "--model", "nexus/glm-5.2", prompt],
        "timeout": 90,
        "env": {**os.environ, "PATH": f"{NODE_PATH}:" + os.environ.get("PATH", "")},
    },
}

BENCHMARKS = {
    "aime_2026": {"category": "reasoning", "target": 99.2, "scoring": "exact"},
    "gpqa_diamond": {"category": "reasoning", "target": 91.2, "scoring": "choice"},
    "hle": {"category": "reasoning", "target": 40.5, "scoring": "semantic"},
    "hmmt_nov_2025": {"category": "reasoning", "target": 94.4, "scoring": "exact"},
    "hmmt_feb_2026": {"category": "reasoning", "target": 92.5, "scoring": "exact"},
    "imo_answerbench": {"category": "reasoning", "target": 91.0, "scoring": "exact"},
    "critpt": {"category": "reasoning", "target": 20.9, "scoring": "semantic"},
    "terminal_bench": {"category": "coding", "target": 81.0, "scoring": "code"},
    "swe_bench_pro": {"category": "coding", "target": 62.1, "scoring": "code"},
    "mcp_atlas": {"category": "agentic", "target": 76.8, "scoring": "task"},
    "tool_decathlon": {"category": "agentic", "target": 48.2, "scoring": "task"},
}


def run_harness(key, prompt):
    h = HARNESSES[key]
    cmd = h["command"](prompt)
    env = h.get("env", os.environ)
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=h["timeout"], env=env)
        return r.stdout, r.returncode == 0
    except subprocess.TimeoutExpired:
        return "", False
    except Exception:
        return "", False


def extract_answer(text):
    """Extract the final answer from a model response."""
    boxed = re.findall(r'\\boxed\{([^}]+)\}', text)
    if boxed:
        return boxed[-1].strip()
    pat = re.search(r'(?:answer is|Answer:|answer:)\s*([^\n.]+)', text, re.IGNORECASE)
    if pat:
        return pat.group(1).strip().rstrip('.')
    lines = [l.strip() for l in text.strip().split('\n') if l.strip()]
    return lines[-1] if lines else text.strip()


def normalize(s):
    """Normalize a string for comparison."""
    s = str(s).strip().lower()
    s = re.sub(r'[\s{}\\]', '', s)
    s = re.sub(r'\\text\{[^}]*\}', '', s)
    s = re.sub(r'\\(?:mathrm|mathbf|text)\{([^}]*)\}', r'\1', s)
    s = s.replace('*', '')
    s = s.replace('^', '')
    return s


def score_exact(response, expected):
    ans = extract_answer(response)
    na, ne = normalize(ans), normalize(expected)
    if na == ne:
        return True
    nums_a = re.findall(r'\d+\.?\d*', ans)
    nums_e = re.findall(r'\d+\.?\d*', expected)
    if nums_a and nums_e and nums_a[-1] == nums_e[-1]:
        return True
    return ne in na or na in ne


def score_choice(response, expected, q):
    ans = extract_answer(response).lower()
    exp = expected.lower()
    if exp in ans:
        return True
    for c in q.get("choices", []):
        if c.lower() != exp and c.lower() in ans:
            return False
    return exp in ans


def score_semantic(response, expected):
    rl = response.lower()
    words = re.findall(r'\b\w+\b', expected.lower())
    key = [w for w in words if len(w) > 2]
    hits = sum(1 for w in key if w in rl)
    return hits >= max(1, len(key) * 0.5)


def score_code(response, expected):
    rl = response.lower()
    has_code = '```' in response or 'def ' in response or 'import' in rl or 'function' in rl or 'class ' in rl
    words = re.findall(r'\b\w+\b', expected.lower())
    key = [w for w in words if len(w) > 3]
    hits = sum(1 for w in key if w in rl)
    return has_code and hits >= max(1, len(key) * 0.3)


def score_task(response, expected):
    rl = response.lower()
    words = re.findall(r'\b\w+\b', expected.lower())
    key = [w for w in words if len(w) > 3]
    hits = sum(1 for w in key if w in rl)
    return hits >= max(1, len(key) * 0.4)


def run_bench(bench_key, harness_key, questions, limit=None):
    bench = BENCHMARKS[bench_key]
    scoring = bench["scoring"]
    if limit:
        questions = questions[:limit]
    
    results = []
    correct = 0
    total = len(questions)
    
    for i, q in enumerate(questions):
        prompt = q.get("question", q.get("task", ""))
        if bench["category"] == "reasoning":
            prompt += "\n\nSolve this step by step. Put your final answer in \\boxed{}."
        elif bench["category"] == "coding":
            prompt += "\n\nProvide a working code solution."
        
        print(f"  [{harness_key}] {bench_key} Q{i+1}/{total}...", end=" ", flush=True)
        
        output, ok = run_harness(harness_key, prompt)
        
        if not ok:
            print("ERR")
            results.append({"id": q.get("id", f"q{i}"), "correct": False, "error": True})
            continue
        
        expected = q.get("answer", q.get("expected", ""))
        
        if scoring == "exact":
            is_ok = score_exact(output, expected)
        elif scoring == "choice":
            is_ok = score_choice(output, expected, q)
        elif scoring == "semantic":
            is_ok = score_semantic(output, expected)
        elif scoring == "code":
            is_ok = score_code(output, expected)
        elif scoring == "task":
            is_ok = score_task(output, expected)
        else:
            is_ok = False
        
        if is_ok:
            correct += 1
            print("OK")
        else:
            print("X")
        
        results.append({
            "id": q.get("id", f"q{i}"),
            "correct": is_ok,
            "extracted": extract_answer(output)[:100],
            "expected": str(expected)[:100],
        })
    
    score = round(correct / total * 100, 1) if total > 0 else 0
    return {
        "benchmark": bench_key,
        "harness": harness_key,
        "category": bench["category"],
        "target": bench["target"],
        "score": score,
        "correct": correct,
        "total": total,
        "results": results,
    }


def main():
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument("--harness", type=str)
    parser.add_argument("--benchmark", type=str)
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--output", type=str, default="results.json")
    args = parser.parse_args()
    
    RESULTS_DIR.mkdir(exist_ok=True)
    harnesses = [args.harness] if args.harness else list(HARNESSES.keys())
    benchmarks = [args.benchmark] if args.benchmark else list(BENCHMARKS.keys())
    
    all_results = []
    
    for bk in benchmarks:
        bf = DATASETS_DIR / f"{bk}.json"
        if not bf.exists():
            continue
        with open(bf) as f:
            questions = json.load(f)
        
        bench = BENCHMARKS[bk]
        print(f"\n{'='*60}")
        print(f"Benchmark: {bk} ({bench['category']}) target={bench['target']}")
        print(f"{'='*60}")
        
        for hk in harnesses:
            print(f"\n  Harness: {HARNESSES[hk]['name']}")
            r = run_bench(bk, hk, questions, args.limit)
            all_results.append(r)
            print(f"  => {r['score']}% ({r['correct']}/{r['total']}) target={r['target']}%")
            
            # Save incremental results
            with open(RESULTS_DIR / args.output, "w") as f:
                json.dump({
                    "timestamp": datetime.now().isoformat(),
                    "model": "glm-5.2",
                    "endpoint": "https://glm-test-glm52-tp4.onenexus-do.cloud/v1",
                    "results": all_results,
                }, f, indent=2)
    
    print(f"\n{'='*80}")
    print(f"{'Benchmark':<22} {'Category':<10} {'Harness':<14} {'Score':>7} {'Target':>7} {'Diff':>7}")
    print(f"{'-'*80}")
    for r in all_results:
        d = r["score"] - r["target"]
        print(f"{r['benchmark']:<22} {r['category']:<10} {r['harness']:<14} {r['score']:>6.1f}% {r['target']:>6.1f}% {d:>+6.1f}")
    print(f"\nResults: {RESULTS_DIR / args.output}")


if __name__ == "__main__":
    main()
