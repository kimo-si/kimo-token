#!/usr/bin/env bash
# Install the kimo-token CLI on a Linux collector (or any Linux machine you want
# to read usage from). One command, no Rust, no Python, no daemon to babysit:
#
#   ./install-linux.sh
#       binary → ~/.local/bin/kimo-token (+ a PATH line in ~/.profile if needed)
#
#   ./install-linux.sh --every 15 --push user@display-host
#       also installs a systemd --user timer that runs `kimo-token export` every
#       15 minutes into ~/kimo-token-sync, and scp's each fresh bundle to the
#       display machine's ~/kimo-token-sync (where the menu-bar engine merges it
#       automatically on its next pass).
#
#   ./install-linux.sh --uninstall
#       removes binary, units and timer. Your data (index, bundles) is kept.
set -euo pipefail

PREFIX="${HOME}/.local/bin"
SYNC_DIR="${HOME}/kimo-token-sync"
UNITS="${HOME}/.config/systemd/user"
EVERY=""
PUSH=""
ORIGIN=""
DAYS=30
UNINSTALL=0

usage() {
  cat <<'EOF'
usage: install-linux.sh [options]

  --every <minutes>   install/replace a timer that exports every N minutes
                      (systemd --user when reachable, else a cron job)
  --push <user@host>  with --every: scp each fresh bundle to that machine's
                      ~/kimo-token-sync (where kimo-token's panel picks it up)
  --origin <name>     origin name for exported bundles (default: hostname;
                      generic cloud hostnames append a short machine-id)
  --days <n>          export window in days (default 30; use 400 once for a
                      first full backfill)
  --prefix <dir>      install directory (default ~/.local/bin)
  --uninstall         remove binary, units and timer; data is kept
  -h, --help          this text
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --every)   EVERY="${2:?--every needs a number of minutes}"; shift 2 ;;
    --push)    PUSH="${2:?--push needs user@host}"; shift 2 ;;
    --origin)  ORIGIN="${2:?--origin needs a name}"; shift 2 ;;
    --days)    DAYS="${2:?--days needs a number}"; shift 2 ;;
    --prefix)  PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
  esac
done

if [[ $UNINSTALL == 1 ]]; then
  systemctl --user disable --now kimo-token-export.timer 2>/dev/null || true
  rm -f "$UNITS/kimo-token-export.timer" "$UNITS/kimo-token-export.service"
  systemctl --user daemon-reload 2>/dev/null || true
  if command -v crontab >/dev/null && crontab -l 2>/dev/null | grep -q 'kimo-token-export-run.sh'; then
    TMP_CRON="$(mktemp)"
    crontab -l 2>/dev/null | grep -v '# kimo-token-sync-export' \
      | grep -v 'kimo-token-export-run.sh' > "$TMP_CRON" || true
    crontab "$TMP_CRON" && echo "cron job removed"
    rm -f "$TMP_CRON"
  fi
  rm -f "$PREFIX/kimo-token" "$PREFIX/kimo-token-sync-push.sh" "$PREFIX/kimo-token-export-run.sh"
  echo "kimo-token removed. Your data is untouched:"
  echo "  index:   ~/.local/share/kimo-token/index.db (or the CLI's --db path)"
  echo "  bundles: $SYNC_DIR"
  exit 0
fi

SRC="$(cd "$(dirname "$0")" && pwd)/kimo-token"
[[ -f "$SRC" ]] || { echo "kimo-token binary not found next to this script ($SRC)" >&2; exit 1; }

mkdir -p "$PREFIX" "$SYNC_DIR"
install -m 0755 "$SRC" "$PREFIX/kimo-token"

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *)
    LINE='export PATH="$HOME/.local/bin:$PATH"'
    if [[ "$PREFIX" == "${HOME}/.local/bin" ]]; then
      grep -qsF "$LINE" "${HOME}/.profile" 2>/dev/null || echo "$LINE" >> "${HOME}/.profile"
      echo "note: added ~/.local/bin to PATH in ~/.profile (new shells pick it up)"
    fi
    ;;
esac

if [[ -z "$ORIGIN" ]]; then
  H="$(hostname 2>/dev/null || echo "linux")"
  H="${H%%.*}"
  case "$H" in
    ubuntu|debian|localhost|instance-*|vps*|"")
      MID=""
      if [[ -f /etc/machine-id ]]; then
        MID="$(cut -c1-6 /etc/machine-id 2>/dev/null || true)"
      elif [[ -f /var/lib/dbus/machine-id ]]; then
        MID="$(cut -c1-6 /var/lib/dbus/machine-id 2>/dev/null || true)"
      fi
      if [[ -n "$MID" ]]; then
        ORIGIN="${H:-linux}-$MID"
      else
        ORIGIN="${H:-linux}"
      fi
      ;;
    *)
      ORIGIN="$H"
      ;;
  esac
fi

echo "installed: $PREFIX/kimo-token ($("$PREFIX/kimo-token" --version))"
echo
echo "verify with the trio:"
echo "  kimo-token detect     # tools found on this machine"
echo "  kimo-token index      # build/refresh the local index"
echo "  kimo-token export     # write ~/kimo-token-sync/kimo-token-$ORIGIN.jsonl.gz"

if [[ -n "$EVERY" ]]; then
  mkdir -p "$UNITS"

  if [[ -n "$PUSH" ]]; then
    # The display machine's engine watches ~/kimo-token-sync; make sure it exists.
    # BatchMode: never prompt here — a missing key is reported, not waited on.
    if ssh -o BatchMode=yes -o ConnectTimeout=5 "$PUSH" "mkdir -p ~/kimo-token-sync" 2>/dev/null; then
      echo "ssh to $PUSH ok (~/kimo-token-sync ensured)"
    else
      echo "note: passwordless ssh to $PUSH is not set up yet."
      echo "      run once, then re-run this installer:  ssh-copy-id $PUSH"
    fi
    cat > "$PREFIX/kimo-token-sync-push.sh" <<PUSH_SCRIPT
#!/usr/bin/env bash
# Generated by install-linux.sh --push: ship the freshest bundle to the
# display machine after each export. -q keeps the journal quiet on success.
set -euo pipefail
scp -q "$SYNC_DIR/kimo-token-$ORIGIN.jsonl.gz" "$SYNC_DIR/kimo-token-$ORIGIN.manifest.json" "$PUSH:kimo-token-sync/"
PUSH_SCRIPT
    chmod +x "$PREFIX/kimo-token-sync-push.sh"
  fi

  # One export pass, shared by the systemd unit and the cron fallback. flock
  # keeps a slow pass from stacking on the next tick; no flock (rare) still
  # exports.
  cat > "$PREFIX/kimo-token-export-run.sh" <<RUNNER
#!/usr/bin/env bash
# Generated by install-linux.sh: export this machine's usage once, then push
# when --push was used. Called by the systemd timer or the cron job.
set -euo pipefail
if command -v flock >/dev/null; then
  exec 9>/tmp/kimo-token-export.lock
  flock -n 9 || exit 0
fi
"$PREFIX/kimo-token" export --days $DAYS --out $SYNC_DIR --origin "$ORIGIN"
if [[ -x "$PREFIX/kimo-token-sync-push.sh" ]]; then "$PREFIX/kimo-token-sync-push.sh"; fi
RUNNER
  chmod +x "$PREFIX/kimo-token-export-run.sh"

  cat > "$UNITS/kimo-token-export.service" <<UNIT
[Unit]
Description=kimo-token — export this machine's usage for sync

[Service]
Type=oneshot
ExecStart=$PREFIX/kimo-token-export-run.sh
UNIT

  cat > "$UNITS/kimo-token-export.timer" <<UNIT
[Unit]
Description=kimo-token export every ${EVERY} min

[Timer]
OnBootSec=2min
OnUnitActiveSec=${EVERY}min
AccuracySec=30s

[Install]
WantedBy=timers.target
UNIT

  if command -v systemctl >/dev/null && systemctl --user daemon-reload 2>/dev/null \
     && systemctl --user enable --now kimo-token-export.timer 2>/dev/null; then
    # A headless box has no login session, so user timers only run with lingering.
    if ! loginctl show-user "$USER" 2>/dev/null | grep -q "Linger=yes"; then
      loginctl enable-linger "$USER" 2>/dev/null \
        || echo "note: if this machine has no interactive login, run once:  sudo loginctl enable-linger $USER"
    fi
    echo
    echo "timer active: every ${EVERY} min → $SYNC_DIR"
    echo "next run:     systemctl --user list-timers kimo-token-export.timer"
    # One export now, so a failure shows up immediately — but a failed first
    # run must not fail the install; the timer retries and the message says how
    # to look.
    if systemctl --user start kimo-token-export.service 2>/dev/null; then
      echo "ran once already: systemctl --user status kimo-token-export.service"
    else
      echo "note: the first export run failed — check it with:  systemctl --user status kimo-token-export.service"
    fi
  else
    # No reachable user manager (container, bare ssh): cron does the same job
    # with no session, no linger and no privileges.
    CRON_LINE=""
    if [[ "$EVERY" =~ ^[0-9]+$ && "$EVERY" -ge 1 && "$EVERY" -lt 60 ]]; then
      CRON_LINE="*/$EVERY * * * *"
    elif [[ "$EVERY" =~ ^[0-9]+$ && "$EVERY" -ge 60 && $((EVERY % 60)) -eq 0 && $((EVERY / 60)) -le 24 ]]; then
      CRON_LINE="0 */$((EVERY / 60)) * * *"
    fi
    if [[ -z "$CRON_LINE" ]]; then
      echo "note: the user manager is not reachable and ${EVERY} min has no cron pattern."
      echo "      with a session available:  systemctl --user enable --now kimo-token-export.timer"
    elif command -v crontab >/dev/null; then
      TMP_CRON="$(mktemp)"
      { crontab -l 2>/dev/null | grep -v '# kimo-token-sync-export' | grep -v 'kimo-token-export-run.sh' || true
        echo "# kimo-token-sync-export every ${EVERY} min"
        echo "$CRON_LINE $PREFIX/kimo-token-export-run.sh >/dev/null"
      } > "$TMP_CRON"
      if crontab "$TMP_CRON" 2>/dev/null; then
        echo "timer: user manager not reachable — installed a cron job instead (every ${EVERY} min)"
      else
        echo "note: could not install cron; with a session available:  systemctl --user enable --now kimo-token-export.timer"
      fi
      rm -f "$TMP_CRON"
    else
      echo "note: no reachable user manager and no crontab on this machine."
      echo "      run once with a session available:  systemctl --user enable --now kimo-token-export.timer"
      echo "      or keep a session open:  sudo loginctl enable-linger $USER"
    fi
    "$PREFIX/kimo-token-export-run.sh" || echo "note: the first run failed; check:  $PREFIX/kimo-token detect"
  fi
fi
