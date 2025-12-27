import argparse
import readline as _
import socket
import struct

import msgpack
import prettytable


def encode_message(message: str) -> bytes:
    message_bytes = message.encode("utf-8")
    message_length = len(message_bytes)

    length_prefix = struct.pack("<Q", message_length)
    return length_prefix + message_bytes


def decode_and_print_table(message_bytes: bytes):
    message: dict = msgpack.unpackb(message_bytes)

    if columns := message.get("Ok"):
        data_columns = columns[0]
        execution_time = columns[1]

        table = prettytable.PrettyTable()

        for column in data_columns:
            column_info = column[0]
            column_data = column[1]

            column_name = None
            alias = column_info[0]
            if alias is not None:
                column_name = alias
            else:
                column_name = next(iter(next(iter(column_info[1].values()))))

            data = []
            for val in column_data:
                if isinstance(val, dict):
                    data.append(next(iter(val.values())))
                else:
                    data.append(val)

            table.add_column(column_name, data)
        print(table)
        print(f"Total rows: {len(columns[0][0][1]) if columns[0][0] else 0}")

        secs = execution_time[0]
        nanos = execution_time[1]
        total_ms = secs * 1000 + nanos / 1_000_000
        print(f"Execution time: {total_ms:.2f} ms")

    elif error := message.get("Err"):
        print(f"Error: {error}")


def run(host: str, port: int):
    try:
        print(f"Connecting to {host}:{port}")
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.connect((host, port))
        print("Connected to TouchHouse server")

        while True:
            sql_command = input("> ")

            encoded_command = encode_message(sql_command)
            _ = sock.send(encoded_command)

            header_bytes = sock.recv(8)
            if not header_bytes:
                print("Connection ended.")
                return
            response_length = struct.unpack("<Q", header_bytes)[0]

            chunks = []
            bytes_received = 0
            while bytes_received < response_length:
                chunk = sock.recv(min(response_length - bytes_received, 4096))
                if not chunk:
                    raise ValueError(
                        "Connection closed before complete message received"
                    )
                chunks.append(chunk)
                bytes_received += len(chunk)
            message_bytes = b"".join(chunks)

            print()
            decode_and_print_table(message_bytes)
            print()

    except ConnectionRefusedError:
        print(f"Couldn't connect to {host}:{port}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    _ = parser.add_argument("host", help="Server host address")
    _ = parser.add_argument("port", type=int, help="Server port number")

    args = parser.parse_args()
    run(args.host, args.port)
