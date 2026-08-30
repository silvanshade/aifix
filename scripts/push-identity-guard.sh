#!/bin/sh
# pre-push: refuse to push as an identity this repo does not expect.
#
# Credential-path confusion (a stale keychain entry, an env leak, an
# embedded token in a scratch clone) surfaces as a push under the wrong
# account, invisible until the audit trail shows it. This guard resolves
# the credential git is about to use and refuses on an unexpected login,
# at the moment of harm rather than after it.
#
# Token credentials resolve with username x-access-token, so the login
# comes from the API using the resolved token, never from the username.
# SSH remotes and helperless setups resolve nothing and pass: the guard
# covers the https credential path, which is where the confusion lives.

allowed="agent-shade silvanshade"

url="${2:-https://github.com}"
case "$url" in https://*) ;; *) exit 0 ;; esac
host_path="${url#https://}"

cred=$(printf 'protocol=https\nhost=%s\n' "${host_path%%/*}" | git credential fill 2>/dev/null)
user=$(printf '%s\n' "$cred" | sed -n 's/^username=//p')
[ -n "$user" ] || exit 0

if [ "$user" = "x-access-token" ]; then
  token=$(printf '%s\n' "$cred" | sed -n 's/^password=//p')
  user=$(curl -sf -H "Authorization: token $token" https://api.github.com/user 2>/dev/null |
    sed -n 's/^ *"login": *"\([^"]*\)".*/\1/p' | head -1)
  [ -n "$user" ] || exit 0
fi

case " $allowed " in
  *" $user "*) exit 0 ;;
  *)
    echo "push-identity-guard: refusing to push as '$user' (allowed: $allowed)" >&2
    echo "push-identity-guard: fix the credential path, then retry" >&2
    exit 1
    ;;
esac
