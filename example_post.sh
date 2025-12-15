#!/bin/bash
# NOTE: Ensure that this file has execution priveleges (chmod +x ./post_example.sh)

# Exit if any command fails
set -e

# This is called with the path to the current archive as the only argument
ARCHIVE_PATH=$1

# Handle archive part here...

# You can optionally remove the archive once you're finished with it
rm -vf "$ARCHIVE_PATH"

exit 0
# POSSIBLE EXIT CODES
#         0 = Success: Continue running
#   1...127 = Failure: Log & continue running
# 128...255 = Panic:   Stop program completely