#!/usr/bin/env python3
"""Call the test app's documented local control API (not its agent MCP)."""
import argparse
import json
import socket


def call(method, params=None, port=7979):
    with socket.create_connection(('127.0.0.1',port),timeout=65) as sock:
        sock.sendall((json.dumps({'id':1,'method':method,'params':params or {}},ensure_ascii=False)+'\n').encode())
        with sock.makefile('rb') as stream:
            line=stream.readline(16*1024*1024)
    response=json.loads(line)
    if not response.get('ok'):
        raise RuntimeError(response.get('error',str(response)))
    return response.get('result')


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('method')
    p.add_argument('params',nargs='?',default='{}')
    a=p.parse_args()
    print(json.dumps(call(a.method,json.loads(a.params)),ensure_ascii=False,indent=2))
