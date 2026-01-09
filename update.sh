#!/usr/bin/env bash

get_script_dir()
{
    local SOURCE_PATH="${BASH_SOURCE[0]}"
    local SYMLINK_DIR
    local SCRIPT_DIR
    # Resolve symlinks recursively
    while [ -L "$SOURCE_PATH" ]; do
        # Get symlink directory
        SYMLINK_DIR="$( cd -P "$( dirname "$SOURCE_PATH" )" >/dev/null 2>&1 && pwd )"
        # Resolve symlink target (relative or absolute)
        SOURCE_PATH="$(readlink "$SOURCE_PATH")"
        # Check if candidate path is relative or absolute
        if [[ $SOURCE_PATH != /* ]]; then
            # Candidate path is relative, resolve to full path
            SOURCE_PATH=$SYMLINK_DIR/$SOURCE_PATH
        fi
    done
    # Get final script directory path from fully resolved source path
    SCRIPT_DIR="$(cd -P "$( dirname "$SOURCE_PATH" )" >/dev/null 2>&1 && pwd)"
    echo "$SCRIPT_DIR"
}

script_dir="$(get_script_dir)"

if [ -d "$script_dir/llama.cpp.git" ]; then
    cd $script_dir/llama.cpp.git
    /usr/bin/git pull
else
    cd $script_dir
    /usr/bin/git clone https://github.com/ggml-org/llama.cpp $script_dir/llama.cpp.git
fi

cd $script_dir

if [ -d "$script_dir/koboldcpp.git" ]; then
    cd $script_dir/koboldcpp.git
    /usr/bin/git pull
else
    cd $script_dir
    /usr/bin/git clone https://github.com/LostRuins/koboldcpp $script_dir/koboldcpp.git
fi

cd $script_dir

docker compose build