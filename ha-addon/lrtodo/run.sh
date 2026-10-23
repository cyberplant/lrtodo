#!/bin/sh
# Punto de entrada: dentro del add-on traduce /data/options.json a variables
# de entorno; fuera de HA (docker común) usa las variables tal cual.
set -e

OPTIONS=/data/options.json
if [ -f "$OPTIONS" ]; then
    opt() { jq -r "$1 // empty" "$OPTIONS"; }
    export LRTODO_DATA_DIR="$(opt .data_dir)"
    export LRTODO_TOKEN="$(opt .token)"
    export LRTODO_HA_INCLUDE="$(jq -r '(.ha_include // []) | join(",")' "$OPTIONS")"
    export LRTODO_HA_EXCLUDE="$(jq -r '(.ha_exclude // []) | join(",")' "$OPTIONS")"
    export LRTODO_HA_POLL="$(opt .poll_secs)"
    LOG="$(opt .log_level)"
    [ -n "$LOG" ] && export LRTODO_LOG="$LOG"
    # Estado de las listas de HA: privado del add-on (no hace falta versionarlo).
    export LRTODO_STATE_DIR=/data/ha-state
    export LRTODO_TRUST_INGRESS=1
fi

: "${LRTODO_DATA_DIR:=/data/lrtodo}"
mkdir -p "$LRTODO_DATA_DIR"
export LRTODO_CONFIG=/dev/null
exec /usr/local/bin/lrtodo --data-dir "$LRTODO_DATA_DIR" serve
