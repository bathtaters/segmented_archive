#!/bin/bash
# NOTE: Ensure that this file has execution priveleges (chmod +x ./post_example.sh)

# Exit if any command fails
set -e

# This is called with the path to the current archive as the only argument
ARCHIVE_PATH=$1
# NOTE: The Post-Script is called once for each archive 'part' as it is created
#       (e.g. /full/path/to/archive.tar.gz.part000 if it is a multi-part archive)
#       The Skip-Script is called once with the base path of the archive that was skipped
#       (e.g. /full/path/to/archive.tar.gz for any archive that is skipped)

# Handle created/skipped archive file here...

# (Post-script only) Optionally remove the archive once you're finished with it
rm -vf "$ARCHIVE_PATH"
# NOTE: For Skip-script, the $ARCHIVE_PATH file will not be created.

exit 0
# POSSIBLE EXIT CODES
#         0 = Success: Continue running
#   1...127 = Failure: Log & continue running
# 128...255 = Panic:   Stop program completely