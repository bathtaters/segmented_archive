#!/bin/bash
# -- DESCRIPTION -- #
# This can be used to restore a segmented backup to its original location.
# usage: ./restore.sh /path/containing/tar/files/ /root/path/to/restore/to/

set -e
shopt -s nullglob

# Constants
TEMP_PATH="/tmp/segmented_archive" # Temporary path to extract tar files
EXT=".tar.gz"                   # Extension of the tar files
PATH_FILE=".seg_arc.path"       # Path file used to place extracted files
REMOVE_TAR_FILES=true           # Whether to remove tar files after extraction

# Arguments
if [ "$#" -ne 2 ]; then
    echo "Usage: $0 /path/containing/tar/files/ /root/path/to/restore/to/" >&2
    exit 1
fi

TAR_PATH=$1 # Path containing the tar files
RESTORE_PATH=$2 # Root path to restore to

# Combine partial files into complete files
combine_parts() {
    local path="$1"

    for first_part in "$path"/*$EXT.part001; do

        # Collect all part files for the base file
        base_file="${first_part%.part001}"
        parts=("$base_file".part*)
        IFS=$'\n' sorted_parts=($(printf '%s\n' "${parts[@]}" | sort -V))

        echo "> Combining ${#sorted_parts[@]} files into $base_file"
        # Append parts one-by-one to avoid excess disk usage
        rm -f "$base_file"
        for part in "${sorted_parts[@]}"; do
            cat "$part" >> "$base_file"
            echo "  Moved $part >> $base_file"
            if [ "$REMOVE_TAR_FILES" = true ]; then rm -f "$part"; fi
        done
    done
}

# Extract the tar files to the RESTORE_PATH
extract_tars() {
    local src_path="$1"
    local dest_root="$2"

    for tar_file in "$src_path"/*$EXT; do
        echo "> Extracting $tar_file..."

        # Extract the tar file to the temp_path
        local temp_folder="$TEMP_PATH/$(basename "$tar_file" $EXT)"
        rm -Rf "$temp_folder"
        mkdir -p "$temp_folder"
        echo "  Created temp folder: $temp_folder"
        tar -xvf "$tar_file" -C "$temp_folder"

        # Panic if the path file does not exist
        if [ ! -f "$temp_folder/$PATH_FILE" ]; then
            echo "  ERROR: Path file ($PATH_FILE) not found in archive: $tar_file" > /dev/stderr
            rm -Rf "$temp_folder"
            echo "  Removed temp folder: $temp_folder"
            exit -1
        fi
        
        # Move the files to the destination path
        local dest_path="$dest_root/$(cat "$temp_folder/$PATH_FILE")"
        echo "  Restoring to $dest_path"
        mkdir -p "$dest_path"
        rsync -av --remove-source-files "$temp_folder/" "$dest_path/"

        rm "$dest_path/$PATH_FILE"
        echo "  Removed path file: $dest_path$PATH_FILE"

        rm -Rf "$temp_folder"
        echo "  Removed temp folder: $temp_folder"

        # Remove the tar file if requested or if it has partial files
        if [[ "$REMOVE_TAR_FILES" = true || -f "$tar_file.part001" ]]; then
            rm -Rf "$tar_file"
            echo "  Removed tar file: $tar_file"
        fi
    done
}

echo "--- Restoring files from $TAR_PATH to $RESTORE_PATH ---"
echo "> Start time: $(date)"
combine_parts "$TAR_PATH"
extract_tars "$TAR_PATH" "$RESTORE_PATH"
echo "> Completed: $(date)"
