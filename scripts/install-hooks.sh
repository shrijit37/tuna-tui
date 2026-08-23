#!/usr/bin/env bash
# Wire the versioned hooks dir into git for this repo.
git config core.hooksPath hooks
echo "core.hooksPath -> hooks ($(git config core.hooksPath))"
