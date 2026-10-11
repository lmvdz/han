"""Event types and shapes of the Matrix binding (§8.2), shared by the appservice and the client."""
ENTRY, RECEIPT, REJECT = "org.han.entry", "org.han.receipt", "org.han.reject"
SPACE, SESSION, STREAM, SIGNAL = "org.han.space", "org.han.session", "org.han.stream", "org.han.signal"
SERVICE_LOCALPART = "han-seq"

def relates(event_id: str) -> dict:
    """The relation that ties a receipt or reject to the entry's Matrix event."""
    return {"rel_type": "m.reference", "event_id": event_id}

def entry_event(entry_id: str, rec: dict | None = None) -> dict:
    """The content of an `org.han.entry` event (E4, P-02): a client's is `{id}` alone; the sequencer's is `{id, kind, author, seq}`. Never a
    header and never a body: those come only from `GET /_han/v1/read`, under P-02."""
    if rec is None: return {"id": entry_id}
    return {"id": entry_id, "kind": rec["entry"]["kind"], "author": rec["entry"]["author"], "seq": rec["receipt"]["seq"]}

def service_user(server: str) -> str: return f"@{SERVICE_LOCALPART}:{server}"

def txn_safe(event_id: str) -> str:
    return event_id.lstrip("$")
