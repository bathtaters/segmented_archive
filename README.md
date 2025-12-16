# Segmented Archive Tool

This will go through a series of folders, archive each segment into a tar.gz archive (splitting based on a max filesize), and run a script on each archive.

- Memory usage is minimized by streaming files and executing post-scripts one-by-one.
- Additionally disk usage can be minimized by removing the archive at the end of your post-script.
- If one segment is the child of another, it will be excluded from the parent segment.
- Split files are suffixed with `.part###`. Example: `archive.tar.gz` => `archive.tar.gz.part001`, `archive.tar.gz.part002`.
- This was created to help with incremental backups to cold storage services. For this you would create a post-script to upload each file part as it is created.

## Usage

1. Generate `config.toml`.
   - Based on [`example_config.toml`](./example_config.toml).
2. Create a `post.sh` that will run for each generated archive.
   - Based on [`example_post.sh`](./example_post.sh).
   - **Don't forget to `chmod +x post.sh`**
   - Return: 0 = success, +int = warning in log, -int = exit with an error.
3. Run the program, with `config.toml` as the only argument.

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
