#!/usr/bin/env bash
# Builds the web app here and uploads the result to Vercel ("prebuilt"
# deployment: Vercel's build machines have no Rust, so nothing is compiled
# there). Settings: vercel.json.
#
#   scripts/deploy-vercel.sh           # production (your site's main address)
#   scripts/deploy-vercel.sh preview   # a separate preview address
#
# First time only: `npx vercel login`, then `npx vercel link` in this folder
# (creates or picks the Vercel project; stored in .vercel/, not committed).
# Needs Node.js (for npx) and what `scripts/build.sh web` needs.
set -euo pipefail
cd "$(dirname "$0")/.."

vercel() { npx --yes vercel@latest "$@"; }

if [ ! -f .vercel/project.json ]; then
    echo "Not linked to a Vercel project yet. Run once: npx vercel login && npx vercel link" >&2
    exit 1
fi

if [ "${1:-production}" = preview ]; then
    environment=preview prod=()
else
    environment=production prod=(--prod)
fi

vercel pull --yes --environment="$environment"   # project settings
vercel build "${prod[@]}"                        # runs scripts/build.sh web → .vercel/output
vercel deploy --prebuilt "${prod[@]}"            # uploads only .vercel/output
