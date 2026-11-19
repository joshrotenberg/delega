#!/bin/sh
# A fake host CLI for the `delega init` and `delega uninstall` tests. The test
# copies this script to a directory as `claude` or `codex`; the name selects
# which host it imitates. It never touches anything outside its own directory
# (or $FAKE_DIR), so no real host configuration is read or written.
#
#   <dir>/<host>.log    one line per call: the arguments
#   <dir>/<host>.cwd    one line per call: the working directory
#   <dir>/state/        what the fake host has registered
#
# Test controls, all environment variables:
#   FAKE_FAIL=word      every call whose arguments include `word` exits 1
#   FAKE_GARBAGE=yes    the JSON listings print text that is not JSON
PATH=/usr/bin:/bin
host=$(basename "$0")
dir=${FAKE_DIR:-$(dirname "$0")}
state="$dir/state"
mkdir -p "$state"
printf '%s\n' "$*" >> "$dir/$host.log"
pwd >> "$dir/$host.cwd"

if [ -n "$FAKE_FAIL" ]; then
    case " $* " in
        *" $FAKE_FAIL "*)
            echo "fake failure: $*" >&2
            exit 1
            ;;
    esac
fi

marketplace="$state/$host.marketplace"
plugin="$state/$host.plugin"
extra="$state/$host.extra"
id="delega@delega-local"

list_marketplaces() {
    if [ "$FAKE_GARBAGE" = yes ]; then
        echo "this is not json"
        return
    fi
    if [ "$host" = claude ]; then
        if [ -f "$marketplace" ]; then
            printf '[{"name":"delega-local","source":"directory","path":"%s","scope":"user"}]\n' "$(cat "$marketplace")"
        else
            echo '[]'
        fi
    else
        if [ -f "$marketplace" ]; then
            printf '{"marketplaces":[{"name":"delega-local","root":"%s"}]}\n' "$(cat "$marketplace")"
        else
            echo '{"marketplaces":[]}'
        fi
    fi
}

list_plugins() {
    if [ "$FAKE_GARBAGE" = yes ]; then
        echo "this is not json"
        return
    fi
    rows=""
    if [ -f "$plugin" ]; then
        if [ "$host" = claude ]; then
            rows=$(printf '{"id":"%s","scope":"user","installPath":"%s"}' "$id" "$(cat "$plugin")")
        else
            rows=$(printf '{"pluginId":"%s","installed":true,"source":{"source":"local","path":"%s"}}' "$id" "$(cat "$plugin")")
        fi
    fi
    if [ -f "$extra" ]; then
        if [ "$host" = claude ]; then
            more='{"id":"other@delega-local","scope":"user","installPath":"/elsewhere"}'
        else
            more='{"pluginId":"other@delega-local","installed":true,"source":{"source":"local","path":"/elsewhere"}}'
        fi
        if [ -n "$rows" ]; then rows="$rows,$more"; else rows="$more"; fi
    fi
    if [ "$host" = claude ]; then
        printf '[%s]\n' "$rows"
    else
        printf '{"installed":[%s]}\n' "$rows"
    fi
}

case "$1 $2" in
    "plugin marketplace")
        case "$3" in
            add)
                printf '%s' "$4" > "$marketplace"
                echo "Added marketplace delega-local from $4"
                ;;
            remove)
                rm -f "$marketplace"
                echo "Removed marketplace delega-local"
                ;;
            list)
                list_marketplaces
                ;;
            *)
                echo "unknown: $*" >&2
                exit 64
                ;;
        esac
        ;;
    "plugin install" | "plugin add")
        if [ ! -f "$marketplace" ]; then
            echo "marketplace delega-local is not configured" >&2
            exit 1
        fi
        printf '%s/plugin/delega' "$(cat "$marketplace")" > "$plugin"
        echo "Installed $id"
        ;;
    "plugin update")
        echo "Updated $id"
        ;;
    "plugin uninstall" | "plugin remove")
        rm -f "$plugin"
        echo "Removed $id"
        ;;
    "plugin list")
        list_plugins
        ;;
    *)
        echo "unknown: $*" >&2
        exit 64
        ;;
esac
