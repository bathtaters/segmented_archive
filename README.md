# Segmented Archive Tool

This will go through a series of folders, archive each segment into a tar.gz archive (splitting based on a max filesize), and run a script on each archive.

- Memory usage is minimized by streaming files and executing post-scripts one-by-one.
- Additionally disk usage can be minimized by removing the archive at the end of your post-script.
- If one segment is the child of another, it will be excluded from the parent segment.
- Split files are suffixed with `.part###`. Example: `archive.tar.gz` => `archive.tar.gz.part001`, `archive.tar.gz.part002`.
- This was created to help with incremental backups to cold storage services. For this you would create a post-script to upload each file part as it is created.
- Can optionally compare segment hashes to a previously generated hash file and only archive segments that have changed.

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
- **`hash_file`**: Path to an existing or future hash file. This will be used to only archive changed segments. _(Default: Archive all)_.
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

## Cross-Compiling

To compile for a NAS requires cross compilation. Here is how I do it on Mac OS for an Intel Synology. _(Steps 1-3 only need to be performed once per system.)_

1. _(If not already)_ Install cross-compilation toolchain for GNU Linux (This cask also contains other versions).

```bash
brew install SergioBenitez/osxct/x86_64-unknown-linux-gnu
```

2. _(If not already)_ Create/add to the cargo `config.toml` file.
   This will configure the new tool to be used when linking to the denoted target.

```toml
# Set for current project: `$project_root/.cargo/config.toml`
# Set globally: `$HOME/.cargo/config.toml`
[target.x86_64-unknown-linux-gnu]
linker = "x86_64-unknown-linux-gnu-gcc"
```

3. _(If not already)_ Add the target version's toolchain to rustup.

```bash
rustup target add x86_64-unknown-linux-gnu
```

4. Build for the target platform.

```bash
cargo build --target x86_64-unknown-linux-gnu --release
```

5. Copy the binary to the remote server from: `$project_root/target/x86_64-unknown-linux-gnu/release/segmented_archive`
