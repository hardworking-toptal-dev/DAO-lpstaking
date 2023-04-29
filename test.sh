set -e
echo "NOTE: does not work (PermissionDenied) in VSCODE terminal, use an external terminal"
cd metapool

RUST_BACKTRACE=1 cargo test -- --nocapture >desk-check.log

echo "-- Output sent to metapool/desk-check.log"
cd -
