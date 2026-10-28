#!/bin/bash
# #956: block before executing the unchanged command. No timer decides release.
IFS= read -r token < "$PDRS_CREATE_FIFO" || exit 97
[ "$token" = release ] || exit 98
exec /bin/bash "$@"
