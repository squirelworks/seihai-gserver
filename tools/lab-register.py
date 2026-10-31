"""Lab only: list a gserver on the lab listserver (127.0.0.1:14900).
seihai-gserver stage 1 has no listserver link yet, so this stands in for it."""
import socket, struct, sys, time, zlib
name, port = sys.argv[1], sys.argv[2]
def gstr(s): return bytes([len(s) + 32]) + s
s = socket.create_connection(('127.0.0.1', 14900))
s.sendall(b'lab-register\n')
f = [name.encode(), b'stage 1 lab', b'English', b'seihai-gserver', b'', b'127.0.0.1', port.encode(), b'127.0.0.1']
body = bytes([22 + 32]) + b''.join(gstr(x) for x in f) + b'\n'
z = zlib.compress(body); s.sendall(struct.pack('>H', len(z)) + z)
while True:
    if not s.recv(4096): break
