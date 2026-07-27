#!/usr/bin/env python3
"""Move Codex history between Official and Remote Agent provider buckets.

When Remote Agent is enabled, existing Official threads can be moved from the
``openai`` bucket into ``nexus-agent`` so they stay in the same history list.
Before the original profile is restored, the helper moves those records back.
Every changed JSONL file and SQLite state database is backed up first.

The script intentionally uses only the Python standard library so it can run
on typical Linux model hosts without installing a package.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import glob
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import stat
import tempfile
from typing import Any, Dict, List, Optional, Set, Tuple


SOURCE_PROVIDER = "nexus-agent"
DEFAULT_OFFICIAL_PROVIDER = "openai"
DEFAULT_OFFICIAL_MODEL = "gpt-5.5"
MIGRATION_VERSION = 1


def _top_level_toml_string(text: str, key: str) -> Optional[str]:
    """Read a quoted top-level TOML string without depending on tomllib."""

    key_pattern = re.compile(
        rf"^\s*{re.escape(key)}\s*=\s*([\"'])(.*?)\1\s*(?:#.*)?$"
    )
    in_table = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_table = True
        if in_table:
            continue
        match = key_pattern.match(line)
        if match:
            value = match.group(2).strip()
            return value or None
    return None


def _load_restore_target(home: Path) -> Tuple[str, str, Path, str]:
    agent_root = home / ".nexus-composer" / "remote-agent"
    marker_path = agent_root / "active-codex-backup.json"
    if not marker_path.is_file():
        raise FileNotFoundError("active Codex profile backup is missing")

    marker = json.loads(marker_path.read_text(encoding="utf-8"))
    config_record = marker.get("files", {}).get("config", {})
    config_text = ""
    config_path_value = config_record.get("path")
    if config_record.get("existed") and isinstance(config_path_value, str):
        config_path = Path(config_path_value)
        if config_path.is_file():
            config_text = config_path.read_text(encoding="utf-8")

    target_provider = (
        _top_level_toml_string(config_text, "model_provider")
        or DEFAULT_OFFICIAL_PROVIDER
    )
    if target_provider == SOURCE_PROVIDER:
        raise RuntimeError(
            "preserved Codex profile still points to nexus-agent; refusing to orphan threads"
        )
    target_model = (
        _top_level_toml_string(config_text, "model") or DEFAULT_OFFICIAL_MODEL
    )
    codex_dir = home / ".codex"
    return target_provider, target_model, codex_dir, config_text


def _collect_jsonl_files(codex_dir: Path) -> List[Path]:
    files: List[Path] = []
    for root_name in ("sessions", "archived_sessions"):
        root = codex_dir / root_name
        if root.is_dir():
            files.extend(path for path in root.rglob("*.jsonl") if path.is_file())
    return sorted(set(files))


def _sqlite_home_from_config(config_text: str, home: Path) -> Optional[Path]:
    value = _top_level_toml_string(config_text, "sqlite_home")
    if not value:
        return None
    if value == "~":
        return home
    if value.startswith("~/"):
        return home / value[2:]
    return Path(value)


def _collect_state_dbs(codex_dir: Path, config_text: str, home: Path) -> List[Path]:
    candidates = [Path(path) for path in glob.glob(str(codex_dir / "state_*.sqlite"))]
    sqlite_home = _sqlite_home_from_config(config_text, home)
    if sqlite_home is None:
        env_home = os.environ.get("CODEX_SQLITE_HOME", "").strip()
        if env_home:
            sqlite_home = Path(env_home).expanduser()
    if sqlite_home is not None:
        candidates.extend(
            Path(path) for path in glob.glob(str(sqlite_home / "state_*.sqlite"))
        )
    return sorted({path.resolve() for path in candidates if path.is_file()})


def _rewrite_session_meta_line(
    line: str, source_provider: str, target_provider: str
) -> Tuple[str, bool]:
    if "session_meta" not in line or "model_provider" not in line:
        return line, False
    try:
        value = json.loads(line)
    except json.JSONDecodeError:
        return line, False
    if value.get("type") != "session_meta":
        return line, False
    payload = value.get("payload")
    if not isinstance(payload, dict) or payload.get("model_provider") != source_provider:
        return line, False
    payload["model_provider"] = target_provider
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")), True


def _rewrite_turn_context_model_line(line: str, target_model: str) -> Tuple[str, bool]:
    if "turn_context" not in line or not target_model:
        return line, False
    try:
        value = json.loads(line)
    except json.JSONDecodeError:
        return line, False
    if value.get("type") != "turn_context":
        return line, False
    payload = value.get("payload")
    if not isinstance(payload, dict) or payload.get("model") == target_model:
        return line, False
    payload["model"] = target_model
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")), True


def _relative_backup_path(path: Path, codex_dir: Path) -> Path:
    try:
        return path.resolve().relative_to(codex_dir.resolve())
    except ValueError:
        safe = str(path.resolve()).lstrip(os.sep).replace(":", "_")
        return Path("external") / safe


def _backup_jsonl(path: Path, codex_dir: Path, backup_root: Path) -> None:
    destination = backup_root / "jsonl" / _relative_backup_path(path, codex_dir)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if not destination.exists():
        shutil.copy2(path, destination)


def _rewrite_jsonl_file(
    path: Path,
    codex_dir: Path,
    backup_root: Path,
    source_provider: str,
    target_provider: str,
    target_model: str,
    normalize_target_model: bool,
) -> bool:
    before = path.stat()
    original = path.read_text(encoding="utf-8")
    segments = original.splitlines(keepends=True)
    belongs_to_source = any(
        _rewrite_session_meta_line(
            segment.rstrip("\r\n"), source_provider, target_provider
        )[1]
        for segment in segments
    )
    belongs_to_target = any(
        _session_meta_provider(segment.rstrip("\r\n")) == target_provider
        for segment in segments
    )
    if not belongs_to_source and not (
        normalize_target_model and belongs_to_target
    ):
        return False
    rewritten: List[str] = []
    changed = False
    for segment in segments:
        line = segment.rstrip("\r\n")
        newline = segment[len(line) :]
        next_line, line_changed = _rewrite_session_meta_line(
            line, source_provider, target_provider
        )
        if not line_changed and (
            belongs_to_source or (normalize_target_model and belongs_to_target)
        ):
            next_line, line_changed = _rewrite_turn_context_model_line(
                line, target_model
            )
        rewritten.append(next_line + newline)
        changed = changed or line_changed
    if not changed:
        return False

    after_read = path.stat()
    if (
        after_read.st_size != before.st_size
        or after_read.st_mtime_ns != before.st_mtime_ns
    ):
        raise RuntimeError(f"Codex session changed during maintenance: {path}")

    _backup_jsonl(path, codex_dir, backup_root)
    after_backup = path.stat()
    if (
        after_backup.st_size != before.st_size
        or after_backup.st_mtime_ns != before.st_mtime_ns
    ):
        raise RuntimeError(f"Codex session changed during backup: {path}")

    fd, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.nexus-", dir=str(path.parent)
    )
    temporary = Path(temporary_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as handle:
            handle.write("".join(rewritten))
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temporary, stat.S_IMODE(before.st_mode))
        os.replace(temporary, path)
        verified = False
        for line in path.read_text(encoding="utf-8").splitlines():
            _, line_still_matches = _rewrite_session_meta_line(
                line, source_provider, target_provider
            )
            if line_still_matches:
                verified = True
                break
        if verified:
            raise RuntimeError(
                f"Codex session provider verification failed after maintenance: {path}"
            )
        if normalize_target_model:
            for line in path.read_text(encoding="utf-8").splitlines():
                if "turn_context" not in line:
                    continue
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if value.get("type") != "turn_context":
                    continue
                payload = value.get("payload")
                if isinstance(payload, dict) and payload.get("model") != target_model:
                    raise RuntimeError(
                        f"Codex session model verification failed after maintenance: {path}"
                    )
    finally:
        if temporary.exists():
            temporary.unlink()
    return True


def _session_meta_provider(line: str) -> Optional[str]:
    if "session_meta" not in line or "model_provider" not in line:
        return None
    try:
        value = json.loads(line)
    except json.JSONDecodeError:
        return None
    if value.get("type") != "session_meta":
        return None
    payload = value.get("payload")
    if not isinstance(payload, dict):
        return None
    provider = payload.get("model_provider")
    return provider if isinstance(provider, str) else None


def _table_columns(connection: sqlite3.Connection, table: str) -> Set[str]:
    return {str(row[1]) for row in connection.execute(f"PRAGMA table_info({table})")}


def _backup_state_db(
    source: sqlite3.Connection,
    db_path: Path,
    codex_dir: Path,
    backup_root: Path,
) -> None:
    destination = backup_root / "state" / _relative_backup_path(db_path, codex_dir)
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        return
    backup_connection = sqlite3.connect(destination)
    try:
        source.backup(backup_connection)
    finally:
        backup_connection.close()


def _migrate_state_db(
    db_path: Path,
    codex_dir: Path,
    backup_root: Path,
    source_provider: str,
    target_provider: str,
    target_model: str,
    normalize_target_model: bool,
) -> int:
    connection = sqlite3.connect(db_path, timeout=5.0)
    try:
        connection.execute("PRAGMA busy_timeout = 5000")
        table_exists = connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name='threads'"
        ).fetchone()
        if not table_exists:
            return 0
        columns = _table_columns(connection, "threads")
        if "model_provider" not in columns:
            return 0
        matching = int(
            connection.execute(
                "SELECT COUNT(*) FROM threads WHERE model_provider = ?",
                (source_provider,),
            ).fetchone()[0]
        )
        target_model_mismatches = 0
        if normalize_target_model and "model" in columns and target_model:
            target_model_mismatches = int(
                connection.execute(
                    "SELECT COUNT(*) FROM threads "
                    "WHERE model_provider = ? AND (model IS NULL OR model != ?)",
                    (target_provider, target_model),
                ).fetchone()[0]
            )
        if matching == 0 and target_model_mismatches == 0:
            return 0

        _backup_state_db(connection, db_path, codex_dir, backup_root)
        connection.execute("BEGIN IMMEDIATE")
        changed = 0
        if matching and "model" in columns and target_model:
            cursor = connection.execute(
                "UPDATE threads SET model_provider = ?, model = ? "
                "WHERE model_provider = ?",
                (target_provider, target_model, source_provider),
            )
            changed += int(cursor.rowcount)
        elif matching:
            cursor = connection.execute(
                "UPDATE threads SET model_provider = ? WHERE model_provider = ?",
                (target_provider, source_provider),
            )
            changed += int(cursor.rowcount)
        if target_model_mismatches:
            cursor = connection.execute(
                "UPDATE threads SET model = ? "
                "WHERE model_provider = ? AND (model IS NULL OR model != ?)",
                (target_model, target_provider, target_model),
            )
            changed += int(cursor.rowcount)
        connection.commit()
        remaining = int(
            connection.execute(
                "SELECT COUNT(*) FROM threads WHERE model_provider = ?",
                (source_provider,),
            ).fetchone()[0]
        )
        if remaining:
            raise RuntimeError(
                f"Codex state DB provider verification failed after maintenance: {db_path}"
            )
        if normalize_target_model and "model" in columns and target_model:
            remaining_model_mismatches = int(
                connection.execute(
                    "SELECT COUNT(*) FROM threads "
                    "WHERE model_provider = ? AND (model IS NULL OR model != ?)",
                    (target_provider, target_model),
                ).fetchone()[0]
            )
            if remaining_model_mismatches:
                raise RuntimeError(
                    f"Codex state DB model verification failed after maintenance: {db_path}"
                )
        return changed
    except Exception:
        connection.rollback()
        raise
    finally:
        connection.close()


def _write_manifest(backup_root: Path, payload: Dict[str, Any]) -> None:
    backup_root.mkdir(parents=True, exist_ok=True)
    destination = backup_root / "manifest.json"
    temporary = destination.with_suffix(".json.tmp")
    temporary.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    os.chmod(temporary, 0o600)
    os.replace(temporary, destination)


def maintain(
    home: Path,
    source_provider: str,
    explicit_target_provider: Optional[str] = None,
    explicit_target_model: Optional[str] = None,
) -> Dict[str, Any]:
    if bool(explicit_target_provider) != bool(explicit_target_model):
        raise ValueError("target provider and target model must be provided together")
    if explicit_target_provider and explicit_target_model:
        target_provider = explicit_target_provider
        target_model = explicit_target_model
        codex_dir = home / ".codex"
        config_path = codex_dir / "config.toml"
        config_text = (
            config_path.read_text(encoding="utf-8")
            if config_path.is_file()
            else ""
        )
        normalize_target_model = True
    else:
        try:
            target_provider, target_model, codex_dir, config_text = (
                _load_restore_target(home)
            )
        except FileNotFoundError:
            return {
                "ok": True,
                "skipped": "no_active_profile_backup",
                "sourceProvider": source_provider,
                "jsonlFiles": 0,
                "stateRows": 0,
            }
        normalize_target_model = False
    if source_provider == target_provider:
        raise ValueError("source and target providers must differ")

    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
    backup_root = (
        home
        / ".nexus-composer"
        / "remote-backups"
        / "codex-history-compat-v1"
        / f"{stamp}-{os.getpid()}"
    )
    lock_path = home / ".nexus-composer" / "remote-agent" / "history-maintenance.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    with lock_path.open("a+", encoding="utf-8") as lock_handle:
        fcntl.flock(lock_handle.fileno(), fcntl.LOCK_EX)
        jsonl_files = _collect_jsonl_files(codex_dir)
        state_dbs = _collect_state_dbs(codex_dir, config_text, home)
        migrated_jsonl = 0
        migrated_rows = 0
        for path in jsonl_files:
            if _rewrite_jsonl_file(
                path,
                codex_dir,
                backup_root,
                source_provider,
                target_provider,
                target_model,
                normalize_target_model,
            ):
                migrated_jsonl += 1
        for db_path in state_dbs:
            migrated_rows += _migrate_state_db(
                db_path,
                codex_dir,
                backup_root,
                source_provider,
                target_provider,
                target_model,
                normalize_target_model,
            )

        result: Dict[str, Any] = {
            "ok": True,
            "version": MIGRATION_VERSION,
            "sourceProvider": source_provider,
            "targetProvider": target_provider,
            "targetModel": target_model,
            "jsonlFiles": migrated_jsonl,
            "stateRows": migrated_rows,
            "stateDatabases": len(state_dbs),
        }
        if migrated_jsonl or migrated_rows:
            result["backupDir"] = str(backup_root)
            _write_manifest(
                backup_root,
                {
                    **result,
                    "completedAt": dt.datetime.now(dt.timezone.utc).isoformat(),
                    "codexDir": str(codex_dir),
                },
            )
        else:
            result["skipped"] = "no_matching_threads"
        return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", default=str(Path.home()))
    parser.add_argument("--source-provider", default=SOURCE_PROVIDER)
    parser.add_argument("--target-provider")
    parser.add_argument("--target-model")
    args = parser.parse_args()
    try:
        result = maintain(
            Path(args.home).expanduser(),
            args.source_provider,
            args.target_provider,
            args.target_model,
        )
        print(json.dumps(result, ensure_ascii=False, separators=(",", ":")))
        return 0
    except Exception as error:  # Keep the control API response concise and actionable.
        print(
            json.dumps(
                {
                    "ok": False,
                    "error": str(error),
                    "sourceProvider": args.source_provider,
                },
                ensure_ascii=False,
                separators=(",", ":"),
            )
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
