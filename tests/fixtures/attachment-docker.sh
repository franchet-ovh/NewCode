#!/bin/sh
# Isolated Docker substitute for attachment tests. Reject all lifecycle commands.
printf '%s %s\n' "$1" "$2" >> "$FOCUS_ATTACH_TEST_ROOT/docker.log"
case "$1" in
    inspect)
        /bin/cat "$FOCUS_ATTACH_TEST_ROOT/inspection.json"
        ;;
    exec)
        if [ "$2" = '--workdir' ]; then
            printf '/workspace/sub project\n'
        elif [ "$2" = '-it' ]; then
            printf 'ATTACHED_TERMINAL\n'
            exec /bin/cat
        else
            exit 0
        fi
        ;;
    *) exit 92 ;;
esac
