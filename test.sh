#!/bin/bash
# Generate a test config and test files
# Then executes the program with the test config

set -e

# Test file locations
TEST_DIR="/tmp/segmented_archive/test"
TEST_CFG="$TEST_DIR/test.toml"

# Get the path to the script itself
SCRIPT_PATH="${BASH_SOURCE[0]}"
SCRIPT_DIR="$(dirname "$(readlink -f "$SCRIPT_PATH")")"


# Handle script exit
on_exit() {
    STATUS=$?

    # On failure, print log file
    echo
    if [ $STATUS -ne 0 ]; then
        echo " --- TEST LOG  --- "
        cat "$TEST_DIR/logs/test_log_"*
        echo
        echo " --- TEST FAILED ($STATUS) --- "
    else
        echo " --- TEST PASSED --- "
    fi

    echo
    echo " --- MANUALLY INSPECT FILES --- "
    open "$TEST_DIR"
    read -p "Press Enter to continue"


    echo
    echo " --- CLEANING UP TEST FILES --- "
    rm -Rf "$TEST_DIR"

    exit $STATUS
}
trap on_exit EXIT


echo " --- RUNNING UNIT TESTS --- "
cargo test

echo
echo " --- GENERATING TEST FILES --- "

rm -Rf "$TEST_DIR"
mkdir -p "$TEST_DIR"

# Create test files
for i in {1..10}; do
    mkdir -p "$TEST_DIR/files/test_dir_$i"
    for j in {1..10}; do
        echo "This is a test file $j in directory test_dir_$i" > "$TEST_DIR/files/test_dir_$i/test_file_$j.txt"
    done
done

for i in {1..100}; do
    echo "This is a test file $i" > "$TEST_DIR/files/test_file_$i.txt"
done

dd if=/dev/urandom of="$TEST_DIR/files/test_dir_1/test_file_large_01.bin" bs=1M count=50

echo "This is a file that should be ignored" > "$TEST_DIR/files/test_dir_5/test_file.ignore"
echo "This is a file that should be ignored" > "$TEST_DIR/files/test_dir_7/test_file.ignore"

mkdir -p "$TEST_DIR/files/test_dir_2/nest_a/nest_b/nest_c"
echo "This is a test file in a nested directory" > "$TEST_DIR/files/test_dir_2/nest_a/nest_b/test_file_nested.txt"
echo "This is a hidden file" > "$TEST_DIR/files/test_dir_2/.hidden_file.txt"

# Create symlink test files
echo "This is a target file for symlinks" > "$TEST_DIR/files/test_dir_3/symlink_target.txt"
mkdir -p "$TEST_DIR/files/test_dir_3/symlink_dir_target"
echo "File inside symlink target directory" > "$TEST_DIR/files/test_dir_3/symlink_dir_target/file.txt"

# Create symlinks
ln -s "symlink_target.txt" "$TEST_DIR/files/test_dir_3/symlink_to_file.txt"
ln -s "symlink_dir_target" "$TEST_DIR/files/test_dir_3/symlink_to_dir"
ln -s "non_existent_file.txt" "$TEST_DIR/files/test_dir_3/broken_symlink.txt"
ln -s "../test_dir_1/test_file_1.txt" "$TEST_DIR/files/test_dir_3/symlink_to_parent.txt"

echo
echo " --- CREATED SYMLINKS (for reference) --- "
ls -la "$TEST_DIR/files/test_dir_3/" | grep -E "^l|total"

# Create test config
echo "output_path = \"$TEST_DIR/archives\"" > "$TEST_CFG"
echo "root_path = \"$TEST_DIR/files\"" >> "$TEST_CFG"
echo "post_script = \"$TEST_DIR/test_script.sh\"" >> "$TEST_CFG"
echo "skip_script = \"$TEST_DIR/skip_script.sh\"" >> "$TEST_CFG"
echo "hash_file = \"$TEST_DIR/logs/test.hash\"" >> "$TEST_CFG"
echo "log_file = \"$TEST_DIR/logs/test_log_%D.log\"" >> "$TEST_CFG"
echo "compression_level = 8" >> "$TEST_CFG"
echo "max_size_bytes = 10485760" >> "$TEST_CFG"
echo "ignore = [\"$TEST_DIR/files/test_dir_4\", \"*.ignore\"]" >> "$TEST_CFG"
echo "" >> "$TEST_CFG"
echo "[segments]" >> "$TEST_CFG"
echo "test_base = \"$TEST_DIR/files\"" >> "$TEST_CFG"
echo "large_file = \"$TEST_DIR/files/test_dir_1/\"" >> "$TEST_CFG"
echo "nested_dir = \"$TEST_DIR/files/test_dir_2/nest_a/nest_b/\"" >> "$TEST_CFG"
echo "symlinks = \"$TEST_DIR/files/test_dir_3/\"" >> "$TEST_CFG"

echo
echo " --- TEST CONFIG: $TEST_CFG --- "
cat "$TEST_CFG"

# Create test script
echo "#!/bin/bash" > "$TEST_DIR/test_script.sh"
echo "FILE_PATH=\$1" >> "$TEST_DIR/test_script.sh"
echo "echo \"Saved archive: \$(ls -l \$FILE_PATH)\"" >> "$TEST_DIR/test_script.sh"
chmod +x "$TEST_DIR/test_script.sh"

# Create skip script that logs skipped segments
echo "#!/bin/bash" > "$TEST_DIR/skip_script.sh"
echo "FILE_PATH=\$1" >> "$TEST_DIR/skip_script.sh"
echo "echo \"Path skipped: \$FILE_PATH\" >> \"$TEST_DIR/skip_log.txt\"" >> "$TEST_DIR/skip_script.sh"
chmod +x "$TEST_DIR/skip_script.sh"

echo
echo " --- TEST SCRIPT: $TEST_DIR/test_script.sh --- "
cat "$TEST_DIR/test_script.sh"
echo
ls -l "$TEST_DIR/test_script.sh"

# Build and run
echo
echo " --- BUILDING AND RUNNING PROGRAM --- "
cargo run "$TEST_CFG"

echo
echo " --- TEST ARCHIVES: $TEST_DIR/archives --- "
ls -R "$TEST_DIR/archives"

log_files=( "$TEST_DIR"/logs/test_log_*.log )
first_log="${log_files[0]}"
echo
echo " --- TEST LOG: $first_log --- "
cat "$first_log"

# Restore test files
echo
echo " --- RESTORING ARCHIVES --- "
"$SCRIPT_DIR/restore.sh" "$TEST_DIR/archives" "$TEST_DIR/restored"

# Re-run to rebuild archive files
cargo run "$TEST_CFG"


echo
echo " --- DIFF BETWEEN ORIGINAL AND RESTORED FILES --- "
diff -r "$TEST_DIR/files" "$TEST_DIR/restored" || true
echo "(You should see: 'No such file or directory' error for broken_symlink, 'test_dir_4,' and 'test_file.ignore' from dirs 5 & 7)"

echo
echo " --- SKIPS UNCHANGED SEGMENTS --- "
echo "x" >> "$TEST_DIR/files/test_file_1.txt"
rm -Rf "$TEST_DIR/archives"
rm -f "$TEST_DIR/skip_log.txt"
cargo run "$TEST_CFG"
ls -l "$TEST_DIR/archives"
echo "(You should only see test_base.tar.gz above)"

echo
echo " --- TESTING SKIP SCRIPT --- "
cat "$TEST_DIR/skip_log.txt"
echo "(You should see symlinks, large_file, and nested_dir skipped)"
