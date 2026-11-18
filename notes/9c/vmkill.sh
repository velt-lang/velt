#!/bin/bash
# Stops my 9c measurement script in the VM by the PID given (its cargo build may finish).
kill "$1" && echo "stopped $1"
