set -e
dnf install -y -q pamtester >/dev/null 2>&1
useradd -m kid; echo 'kid:secret' | chpasswd

# Two services, identical except for the leading dash, both naming a module
# that does not exist — the situation after a package removal or an unmerged
# sysext.
cat > /etc/pam.d/tb-plain <<'P'
auth     requisite    pam_timebandits.so
auth     required     pam_unix.so
account  required     pam_timebandits.so
account  required     pam_unix.so
P
cat > /etc/pam.d/tb-dash <<'P'
-auth     requisite    pam_timebandits.so
auth      required     pam_unix.so
-account  required     pam_timebandits.so
account   required     pam_unix.so
P

for svc in tb-plain tb-dash; do
  for op in authenticate acct_mgmt; do
    printf '%-10s %-14s ' "$svc" "$op"
    if echo secret | pamtester "$svc" kid "$op" >/dev/null 2>&1; then
      echo "PASS  (login works)"
    else
      echo "FAIL  (locked out)"
    fi
  done
done
