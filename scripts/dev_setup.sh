#!/bin/bash
# This script sets up the environment for the Libra build by installing necessary dependencies.
#
# Usage ./dev_setup.sh <options>
#   v - verbose, print all statements

SCRIPT_PATH="$( cd "$( dirname "${BASH_SOURCE[0]}" )" >/dev/null 2>&1 && pwd )"
cd "$SCRIPT_PATH/.."

set -e
OPTIONS="$1"

if [[ $OPTIONS == *"v"* ]]; then
	set -x
fi

if [ ! -f Cargo.toml ]; then
	echo "Unknown location. Please run this from the libra repository. Abort."
	exit 1
fi

PACKAGE_MANAGER=
if [[ "$OSTYPE" == "linux-gnu" ]]; then
	if which yum &>/dev/null; then
		PACKAGE_MANAGER="yum"
	elif which apt-get &>/dev/null; then
		PACKAGE_MANAGER="apt-get"
	elif which pacman &>/dev/null; then
		PACKAGE_MANAGER="pacman"
	else
		echo "Unable to find supported package manager (yum, apt-get, or pacman). Abort"
		exit 1
	fi
elif [[ "$OSTYPE" == "darwin"* ]]; then
	if which brew &>/dev/null; then
		PACKAGE_MANAGER="brew"
	else
		echo "Missing package manager Homebrew (https://brew.sh/). Abort"
		exit 1
	fi
else
	echo "Unknown OS. Abort."
	exit 1
fi

cat <<EOF
Welcome to Libra via Solana!

This script will download and install the necessary dependencies needed to
build Libra Core targeting Solana. This includes:
	* CMake, protobuf, go (for building protobuf)

EOF

if [[ $"$PACKAGE_MANAGER" == "apt-get" ]]; then
	echo "Updating apt-get......"
	sudo apt-get update
fi

echo "Installing CMake......"
if which cmake &>/dev/null; then
	echo "CMake is already installed"
else
	if [[ "$PACKAGE_MANAGER" == "yum" ]]; then
		sudo yum install cmake -y
	elif [[ "$PACKAGE_MANAGER" == "apt-get" ]]; then
		sudo apt-get install cmake -y
	elif [[ "$PACKAGE_MANAGER" == "pacman" ]]; then
		sudo pacman -Syu cmake --noconfirm
	elif [[ "$PACKAGE_MANAGER" == "brew" ]]; then
		brew install cmake
	fi
fi

echo "Installing Go......"
if which go &>/dev/null; then
	echo "Go is already installed"
else
	if [[ "$PACKAGE_MANAGER" == "yum" ]]; then
		sudo yum install golang -y
	elif [[ "$PACKAGE_MANAGER" == "apt-get" ]]; then
		sudo apt-get install golang -y
	elif [[ "$PACKAGE_MANAGER" == "pacman" ]]; then
		sudo pacman -Syu go --noconfirm
	elif [[ "$PACKAGE_MANAGER" == "brew" ]]; then
		brew install go
	fi
fi

echo "Installing Protobuf......"
if which protoc &>/dev/null; then
  echo "Protobuf is already installed"
else
	if [[ "$OSTYPE" == "linux-gnu" ]]; then
		if ! which unzip &>/dev/null; then
			echo "Installing unzip......"
			if [[ "$PACKAGE_MANAGER" == "yum" ]]; then
				sudo yum install unzip -y
			elif [[ "$PACKAGE_MANAGER" == "apt-get" ]]; then
				sudo apt-get install unzip -y
			elif [[ "$PACKAGE_MANAGER" == "pacman" ]]; then
				sudo pacman -Syu unzip --noconfirm
			fi
		fi
		PROTOC_VERSION=3.8.0
		PROTOC_ZIP=protoc-$PROTOC_VERSION-linux-x86_64.zip
		curl -OL https://github.com/google/protobuf/releases/download/v$PROTOC_VERSION/$PROTOC_ZIP
		sudo unzip -o $PROTOC_ZIP -d /usr/local bin/protoc
		sudo unzip -o $PROTOC_ZIP -d /usr/local include/*
		rm -f $PROTOC_ZIP
		echo "protoc is installed to /usr/local/bin/"
	else
		brew install protobuf
	fi
fi

cat <<EOF

Finished installing all dependencies.

You should now be able to build the project by running:
       cargo test --manifest-path=programs/move_loader_program/Cargo.toml
EOF
