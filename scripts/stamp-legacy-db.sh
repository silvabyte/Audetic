#!/usr/bin/env bash
#
# One-time move of a pre-`user_version` Audetic database onto the numbered
# migration chain. Run it after `git pull` and before `make install`.
#
# It stops the daemon, checks that the database is legacy schema version 3 and
# matches migration 0001, backs it up, then drops the old marker tables and sets
# `PRAGMA user_version = 1`. The next daemon start applies migration 2 onward.
#
# Usage: scripts/stamp-legacy-db.sh [--dry-run] [DB_PATH]
#
# The daemon is only stopped when DB_PATH is the default database.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
baseline="$repo_root/crates/audetic/src/db/migrations/0001_baseline.sql"

dry_run=0
if [[ "${1:-}" == "--dry-run" ]]; then
  dry_run=1
  shift
fi

case "$(uname -s)" in
  Darwin) default_db="$HOME/Library/Application Support/audetic/audetic.db" ;;
  Linux) default_db="${XDG_DATA_HOME:-$HOME/.local/share}/audetic/audetic.db" ;;
  *)
    echo "✗ Unsupported platform: $(uname -s)" >&2
    exit 1
    ;;
esac
db="${1:-$default_db}"
stopped=0

die() {
  echo "✗ $*" >&2
  if ((stopped)); then
    echo "  The daemon is stopped and the database is unchanged. Restart the installed build with:" >&2
    case "$(uname -s)" in
      Darwin) echo "  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/ai.audetic.daemon.plist" >&2 ;;
      Linux) echo "  systemctl --user start audeticd.service" >&2 ;;
    esac
  fi
  exit 1
}

read_db() {
  sqlite3 -readonly -bail "$db" "$@"
}

command -v sqlite3 >/dev/null || die "sqlite3 is not installed"
[[ -f "$baseline" ]] || die "Missing $baseline"
if [[ ! -f "$db" ]]; then
  echo "No database at $db; nothing to do."
  exit 0
fi

version=$(read_db "PRAGMA user_version;")
if ((version > 0)); then
  echo "$db is already at user_version $version; nothing to do."
  exit 0
fi

has_marker=$(read_db "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'audio_notes_schema';")
[[ "$has_marker" == 1 ]] || die "$db has no audio_notes_schema table, so it is not a legacy Audetic database"
legacy=$(read_db "SELECT version FROM audio_notes_schema WHERE singleton = 1;")
[[ "$legacy" == 3 ]] || die "Legacy schema version is $legacy, expected 3. Run a build from before PR 72 once (it upgrades itself to 3), then rerun this script."

shape_sql="
SELECT m.name, p.name, p.type, p.\"notnull\", ifnull(p.dflt_value, ''), p.pk
FROM sqlite_master m JOIN pragma_table_info(m.name) p
WHERE m.type = 'table' AND m.name NOT LIKE 'sqlite_%'
  AND m.name NOT IN ('audio_notes_schema', 'audio_note_migration_sources')
ORDER BY 1, 2;
SELECT 'index', name, tbl_name FROM sqlite_master
WHERE type = 'index' AND sql IS NOT NULL
ORDER BY 2;"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
sqlite3 -bail "$work/expected.db" <"$baseline"
if ! diff <(read_db "$shape_sql") <(sqlite3 -readonly "$work/expected.db" "$shape_sql") >"$work/shape.diff"; then
  cat "$work/shape.diff" >&2
  die "Schema does not match migration 0001 (< this database, > 0001_baseline.sql)"
fi
[[ "$(read_db "PRAGMA integrity_check;")" == ok ]] || die "integrity_check failed on $db"

if ((dry_run)); then
  echo "Dry run: $db is legacy version 3, matches migration 0001, and would be stamped at user_version 1."
  exit 0
fi

if [[ "$db" == "$default_db" ]]; then
  echo "Stopping the Audetic daemon..."
  case "$(uname -s)" in
    Darwin) launchctl bootout "gui/$(id -u)/ai.audetic.daemon" 2>/dev/null || true ;;
    Linux) systemctl --user stop audeticd.service 2>/dev/null || true ;;
  esac
  stopped=1
  for _ in $(seq 1 30); do
    pgrep -x audeticd >/dev/null || break
    sleep 1
  done
  if pgrep -x audeticd >/dev/null; then
    die "audeticd is still running; stop it and rerun"
  fi
fi

backup="$db.pre-stamp-$(date +%Y%m%d%H%M%S).sqlite3"
read_db ".backup '$backup'"
[[ "$(sqlite3 -readonly "$backup" "PRAGMA integrity_check;")" == ok ]] || die "Backup at $backup failed integrity_check"
echo "Backed up to $backup"

sqlite3 -bail "$db" "
BEGIN IMMEDIATE;
DROP TABLE audio_notes_schema;
DROP TABLE IF EXISTS audio_note_migration_sources;
PRAGMA user_version = 1;
COMMIT;"
[[ "$(read_db "PRAGMA user_version;")" == 1 ]] || die "Stamp did not take; restore from $backup"

echo "Stamped $db at user_version 1."
if ((stopped)); then
  echo "Next: make install. The new daemon applies migration 2 and writes audetic.db.pre-v2.sqlite3."
fi
