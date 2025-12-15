# Segmented Backup Tool

This will go through a series of folders, archive their contents (splitting based on a max filesize), then run a script on each archived file.

It will attempt to keep memory usage minimal by streaming files and executing post-scripts one-by-one. If you wish to keep disk usage to a minimum, you may remove the archive in your post-script once you're done processing it.

This was created to help with incremental backups to cold storage services. For this you would use the post-script to upload each file part before deleting it.

## Usage

1. Generate `config.toml` (Based on `example_config.toml`).
2. Create a `post.sh` to execute for each generated archive (Based on `example_post.sh`).
3. Execute the program, with the `config.toml` file as the only argument.

```bash
./segment_backup ./config.toml
```

## Config `.toml`

A config file is required to run this program.
The easiest way to get started is by copying the [example config](./example_config.toml).

### Fields

All fields (unless otherwise noted) are optional strings.

- **`output_path`**: Folder to save all generated archives in _(Default: `/tmp`)_.
- **`root_path`**: Relative base path to use when restoring _(Default: `/`)_.
- **`post_script`**: Script to execute after each file segment is closed _(Default: No script)_.
- **`log_file`**: Path to generate logs. `%D` is replaced with a date-stamp _(Default: No log)_.
- **`compression_level`**: Level of GZip compression to use _(`0 - 9 uint`, Default: `6`)_.
- **`max_size_bytes`**: Maximum file size before a split, in bytes _(`uint`, Default: No splitting)_.
- **`segments`**: List of archive names (keys) and file-paths (values) to archive _(`section of key/value pairs`, Required)_.

---

# Restoring Backups

Also included is a bash script to help restore files generated using this program that will place files from the archives back into place, while leaving any surrounding files alone.

## Usage

```bash
./restore.sh /archive/path/ /restore/path/
```

- **`/archive/path/`**: Path to a directory containing the files output by this script.
- **`/restore/path/`**: `root_path` value from `config.toml` (Or `/` if no root path was set)

## Advanced Options

There are some additional advanced options that can be set at the top of the [restore script](./restore.sh). (Changing these can break the script!)

- **`TEMP_PATH`**: Temporary path to extract backups to.
- **`EXT`**: Extension of the backup files.
- **`PATH_FILE`**: Path file used to place extracted files
- **`REMOVE_TAR_FILES`**: Whether to remove tar files after extraction
