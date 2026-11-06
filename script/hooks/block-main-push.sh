#!/bin/sh
# pre-push: refuse pushes that update or delete the remote main branch.
# Git passes one "<local ref> <local sha> <remote ref> <remote sha>" line per pushed ref on stdin.
status=0
while read -r _local_ref _local_sha remote_ref _remote_sha; do
  if [ "$remote_ref" = "refs/heads/main" ]; then
    echo "Refusing to push to 'main'. Push a branch and open a pull request instead." >&2
    status=1
  fi
done
exit "$status"
