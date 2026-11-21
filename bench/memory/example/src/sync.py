"""Marmot sync: one websocket connection, the server pushes change announcements."""
RECONNECT_DELAY_SECONDS = 5


def connect(url):
    return {"url": url, "mode": "push"}
