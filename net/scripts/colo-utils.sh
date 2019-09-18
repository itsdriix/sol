#!/usr/bin/env bash

declare -r SOLANA_LOCK_FILE="/home/solana/.solana.lock"

declare __COLO_TODO_PARALLELIZE=false

__colo_here="$(dirname "$BASH_SOURCE")"
# Load colo resource specs
export __COLO_RES_N=0
export __COLO_RES_HOSTNAME=()
export __COLO_RES_IP=()
export __COLO_RES_IP_PRIV=()
export __COLO_RES_CPU_CORES=()
export __COLO_RES_RAM_GB=()
export __COLO_RES_STORAGE_TYPE=()
export __COLO_RES_STORAGE_CAP_GB=()
export __COLO_RES_ADD_STORAGE_TYPE=()
export __COLO_RES_ADD_STORAGE_CAP_GB=()
export __COLO_RES_MACHINE=()

export __COLO_RESOURCES_LOADED=false
__colo_load_resources() {
  if ! ${__COLO_RESOURCES_LOADED}; then
    while read -r LINE; do
      IFS='|' read -r H I PI C M ST SC AST ASC G Z <<<"$LINE"
      __COLO_RES_HOSTNAME+=( "$H" )
      __COLO_RES_IP+=( "$I" )
      __COLO_RES_IP_PRIV+=( "$PI" )
      __COLO_RES_CPU_CORES+=( "$C" )
      __COLO_RES_RAM_GB+=( "$M" )
      __COLO_RES_STORAGE_TYPE+=( "$ST" )
      __COLO_RES_STORAGE_CAP_GB+=( "$SC" )
      __COLO_RES_ADD_STORAGE_TYPE+=( "$(tr ',' $'\v' <<<"$AST")" )
      __COLO_RES_ADD_STORAGE_CAP_GB+=( "$(tr ',' $'\v' <<<"$ASC")" )
      __COLO_RES_MACHINE+=( "$G" )
      __COLO_RES_ZONE+=( "$Z" )
      __COLO_RES_N=$((__COLO_RES_N+1))
    done < <(sort -nt'|' -k10,10 "$__colo_here"/colo_nodes)
    __COLO_RESOURCES_LOADED=true
  fi
}

declare __COLO_RES_AVAILABILITY_CACHED=false
declare -ax __COLO_RES_AVAILABILITY
__colo_load_availability() {
  declare USE_CACHE=${1:-${__COLO_RES_AVAILABILITY_CACHED}}
  declare LINE PRIV_IP STATUS LOCK_USER I IP HOST_NAME ZONE INSTNAME
  if ! $USE_CACHE; then
    __COLO_RES_AVAILABILITY=()
    __COLO_RES_REQUISITIONED=()
    while read -r LINE; do
      IFS=$'\v' read -r PRIV_IP STATUS LOCK_USER INSTNAME <<< "$LINE"
      I=$(__colo_res_index_from_ip "$PRIV_IP")
      IP="${__COLO_RES_IP[$I]}"
      HOST_NAME="${__COLO_RES_HOSTNAME[$I]}"
      ZONE="${__COLO_RES_ZONE[$I]}"
      __COLO_RES_AVAILABILITY+=( "$(echo -e "$HOST_NAME\v$IP\v$PRIV_IP\v$STATUS\v$ZONE\v$LOCK_USER\v$INSTNAME")" )
    done < <(__colo_node_status_all | sort -t $'\v' -k1)
    __COLO_RES_AVAILABILITY_CACHED=true
  fi
}

__colo_print_availability() {
  declare HOST_NAME IP PRIV_IP STATUS ZONE LOCK_USER INSTNAME
  if ! $__COLO_TODO_PARALLELIZE; then
    __colo_load_resources
    __colo_load_availability false
  fi
  for AVAIL in "${__COLO_RES_AVAILABILITY[@]}"; do
    IFS=$'\v' read -r HOST_NAME IP PRIV_IP STATUS ZONE LOCK_USER INSTNAME <<<"$AVAIL"
    printf "%-30s | publicIp=%-16s privateIp=%s status=%s zone=%s inst=%s\n" "$HOST_NAME" "$IP" "$PRIV_IP" "$STATUS" "$ZONE" "$INSTNAME"
  done
}

__colo_res_index_from_ip() {
  declare IP="$1"
  for i in "${!__COLO_RES_IP_PRIV[@]}"; do
    if [ "$IP" = "${__COLO_RES_IP_PRIV[$i]}" ]; then
      echo "$i"
      return 0
    fi
  done
  return 1
}

__colo_instance_run() {
  declare IP=$1
  declare CMD="$2"
  declare OUT
  OUT=$(ssh -l solana -o "ConnectTimeout=10" "$IP" "$CMD" 2>&1)
  declare RC=$?
  while read -r LINE; do
    echo -e "$IP\v$RC\v$LINE"
  done <<< "$OUT"
  return $RC
}

__colo_instance_run_foreach() {
  declare CMD
  if test 1 -eq $#; then
    CMD="$1"
    declare IPS=()
    for I in $(seq 0 $((__COLO_RES_N-1))); do
      IPS+=( "${__COLO_RES_IP_PRIV[$I]}" )
    done
    set "${IPS[@]}" "$CMD"
  fi
  CMD="${*: -1}"
  for I in $(seq 0 $(($#-2))); do
    declare IP="$1"
    __colo_instance_run "$IP" "$CMD" &
    shift
  done

  wait
}

__colo_whoami() {
  declare ME LINE SOL_USER
  while read -r LINE; do
    declare IP RC
    IFS=$'\v' read -r IP RC SOL_USER <<< "$LINE"
    if [ "$RC" -eq 0 ]; then
      if [ -z "$ME" ] || [ "$ME" = "$SOL_USER" ]; then
        ME="$SOL_USER"
      else
        echo "Found conflicting username \"$SOL_USER\" on $IP, expected \"$ME\"" 1>&2
      fi
    fi
  done < <(__colo_instance_run_foreach "[ -n \"\$SOLANA_USER\" ] && echo \"\$SOLANA_USER\"")
  echo "$ME"
}

__COLO_SOLANA_USER=""
__colo_get_solana_user() {
  if [ -z "$__COLO_SOLANA_USER" ]; then
    __COLO_SOLANA_USER=$(__colo_whoami)
  fi
  echo "$__COLO_SOLANA_USER"
}

___colo_node_status_script() {
  cat <<EOF
  exec 3>&2
  exec 2>/dev/null  # Suppress stderr as the next call to exec fails most of
                    # the time due to $SOLANA_LOCK_FILE not existing and is running from a
                    # subshell where normal redirection doesn't work
  exec 9<"$SOLANA_LOCK_FILE" && flock -s 9 && . "$SOLANA_LOCK_FILE" && exec 9>&-
  echo -e "\$SOLANA_LOCK_USER\\v\$SOLANA_LOCK_INSTANCENAME"
  exec 2>&3 # Restore stderr
EOF
}

___colo_node_status_result_normalize() {
  declare IP RC US BY INSTNAME
  declare ST="DOWN"
  IFS=$'\v' read -r IP RC US INSTNAME <<< "$1"
  if [ "$RC" -eq 0 ]; then
    if [ -n "$US" ]; then
      BY="$US"
      ST="HELD"
    else
      ST="FREE"
    fi
  fi
  echo -e $"$IP\v$ST\v$BY\v$INSTNAME"
}

__colo_node_status() {
  declare IP="$1"
  ___colo_node_status_result_normalize "$(__colo_instance_run "$IP" "$(___colo_node_status_script)")"
}

__colo_node_status_all() {
  declare LINE
  while read -r LINE; do
    ___colo_node_status_result_normalize "$LINE"
  done < <(__colo_instance_run_foreach "$(___colo_node_status_script)")
}

# TODO: As part of __COLO_TOOD_PARALLELIZE this list will need to be maintained
# in a lockfile to work around `cloud_CreateInstance` being called in the
# background for fullnodes
export __COLO_RES_REQUISITIONED=()
__colo_node_requisition() {
  declare IP=$1
  declare INSTANCE_NAME=$2

  declare INDEX=$(__colo_res_index_from_ip "$IP")
  declare RC=false

  __colo_instance_run "$IP" "$(
cat <<EOF
  if [ ! -f "$SOLANA_LOCK_FILE" ]; then
    exec 9>>"$SOLANA_LOCK_FILE"
    flock -x -n 9 || exit 1
    [ -n "\$SOLANA_USER" ] && {
      echo "export SOLANA_LOCK_USER=\$SOLANA_USER"
      echo "export SOLANA_LOCK_INSTANCENAME=$INSTANCE_NAME"
      echo "[ -v SSH_TTY -a -f \"\${HOME}/.solana-motd\" ] && cat \"\${HOME}/.solana-motd\" 1>&2"
    } >&9 || ( rm "$SOLANA_LOCK_FILE" && false )
    9>&-
    cat > /solana-scratch/id_ecdsa <<EOK
$(cat "$sshPrivateKey")
EOK
    cat > /solana-scratch/id_ecdsa.pub <<EOK
$(cat "${sshPrivateKey}.pub")
EOK
    chmod 0600 /solana-scratch/id_ecdsa
    cat > /solana-scratch/authorized_keys <<EOAK
$("$__colo_here"/add-datacenter-solana-user-authorized_keys.sh 2> /dev/null)
$(cat "${sshPrivateKey}.pub")
EOAK
    cp /solana-scratch/id_ecdsa "\${HOME}/.ssh/id_ecdsa"
    cp /solana-scratch/id_ecdsa.pub "\${HOME}/.ssh/id_ecdsa.pub"
    cp /solana-scratch/authorized_keys "\${HOME}/.ssh/authorized_keys"
    cat > "\${HOME}/.solana-motd" <<EOM


$(printNetworkInfo)
$(creationInfo)
EOM

    # XXX: Stamp creation MUST be last!
    touch /solana-scratch/.instance-startup-complete
  else
    false
  fi
EOF
  )"
  if [[ 0 -eq $? ]]; then
    __COLO_RES_REQUISITIONED+=("$INDEX")
    RC=true
  fi
  $RC
}

__colo_node_is_requisitioned() {
  declare INDEX="$1"
  declare REQ
  declare RC=false
  for REQ in "${__COLO_RES_REQUISITIONED[@]}"; do
    if [[ $REQ -eq $INDEX ]]; then
      RC=true
      break
    fi
  done
  $RC
}

__colo_machine_types_compatible() {
  declare MAYBE_MACH="$1"
  declare WANT_MACH="$2"
  declare COMPATIBLE=false
  # XXX: Colo machine types are just GPU count ATM...
  if [[ "$MAYBE_MACH" -ge "$WANT_MACH" ]]; then
    COMPATIBLE=true
  fi
  $COMPATIBLE
}

__colo_node_free() {
  declare IP=$1
  __colo_instance_run "$IP" "$(
cat <<EOF
  RC=false
  if [ -f "$SOLANA_LOCK_FILE" ]; then
    exec 9<>"$SOLANA_LOCK_FILE"
    flock -x -n 9 || exit 1
    . "$SOLANA_LOCK_FILE"
    if [ "\$SOLANA_LOCK_USER" = "\$SOLANA_USER" ]; then
      git clean -qxdff
      rm -f /solana-scratch/* /solana-scratch/.[^.]*
      cat > "\${HOME}/.ssh/authorized_keys" <<EOAK
$("$__colo_here"/add-datacenter-solana-user-authorized_keys.sh 2> /dev/null)
EOAK
      RC=true
    fi
    9>&-
  fi
  \$RC
EOF
  )"
}


