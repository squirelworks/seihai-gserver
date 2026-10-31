# Real clients under Wine (loopback only)

How the stage 1 Wine run was done (2026-10-31). Everything runs in a network
namespace whose only interface is `lo`, with the old Graal host names pointed
at `127.0.0.1`, so a client can reach nothing outside.

```
apt install wine wine32:i386 xdotool tcpdump imagemagick xvfb iproute2 p7zip-full
# namespace: lo only, a hosts file bind-mounted over /etc/hosts
unshare -n -m --fork sh -c 'ip link set lo up; mount --bind ./hosts /etc/hosts; echo $$ > ns.pid; exec sleep infinity' &
E="nsenter -t $(cat ns.pid) -n -m"
$E Xvfb :9 -screen 0 1024x768x24 &
$E python3 -I listserver.py run/listserver.log &          # the lab listserver (handoff folder)
$E seihai-gserver --world world --verbose &               # create_accounts = true in the lab world
$E python3 -I tools/lab-register.py "Seihai Rust" 14802 & # lists us; stage 1 has no listserver link
$E tcpdump -U -i lo -w run/lab.pcap 'port 14802 or portrange 14921-14923' &
$E sh -c 'cd CLIENTDIR && DISPLAY=:9 WINEPREFIX=$PWD/../wp WINEDEBUG=-all wine Graal.exe' &
```

`hosts` holds `127.0.0.1 localhost` and the listserver host names the
clients carry. Drive the windows with `xdotool` and check them with
`import -window root shot.png`. Set `nodirectinput=true` in `graalconfig.txt`
or the keyboard does nothing. Text fields do not take `ctrl+a`: click, `End`,
then `BackSpace`s.

- 1.39: "One Player Online", server `127.0.0.1`, port 14802, no listserver.
- 2.05 rev 2: the 2.05 exe in a copy of a 2.x game folder; list on 14921.
- 2.17.1, 2.22, 2.31: the Graal Reborn 2.22.0 folder; list on 14922.
- 3.04: the Linux `graalexe` runs natively with its own `lib/`
  (`LD_LIBRARY_PATH=./lib ./graalexe`); the installer was not in the
  project files on 2026-10-31.
