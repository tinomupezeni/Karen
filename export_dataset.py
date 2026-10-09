#!/usr/bin/env python3
import sqlite3
import json
import os
from pathlib import Path

def export_finetune_dataset():
    # Path to the karen memory DB
    db_path = Path.home() / ".local" / "share" / "karen-assistant" / "memory.db"
    if not db_path.exists():
        print(f"Error: Database not found at {db_path}")
        return

    output_path = Path("karen_finetune_dataset.jsonl")
    
    conn = sqlite3.connect(db_path)
    conn.row_factory = sqlite3.Row
    cursor = conn.cursor()

    # Get all conversations
    cursor.execute("SELECT id FROM conversations ORDER BY started_at ASC")
    conversations = cursor.fetchall()

    with open(output_path, "w") as f:
        for conv in conversations:
            conv_id = conv["id"]
            
            # Fetch messages for this conversation
            cursor.execute(
                "SELECT role, content FROM messages WHERE conversation_id = ? ORDER BY recorded_at ASC",
                (conv_id,)
            )
            messages = cursor.fetchall()
            
            if not messages:
                continue

            # Format for fine-tuning
            conversation_data = {
                "messages": [
                    {
                        "role": "system", 
                        "content": "You are Karen, a hyper-competent but mildly exasperated AI desktop assistant. Your responses are brief, deadpan, and slightly condescending, yet perfectly accurate."
                    }
                ]
            }

            for msg in messages:
                # We skip the auto-injected system timestamps in fine-tuning, or keep them. 
                # For pure persona training, raw content is best.
                role = msg["role"]
                content = msg["content"]
                
                # Optionally strip the "[just now]: " prefixes from history for clean training
                if content.startswith("["):
                    # Find first ]:
                    idx = content.find("]: ")
                    if idx != -1:
                        content = content[idx+3:]

                conversation_data["messages"].append({
                    "role": role,
                    "content": content
                })

            f.write(json.dumps(conversation_data) + "\n")

    print(f"✅ Successfully exported {len(conversations)} conversations to {output_path}")

if __name__ == "__main__":
    export_finetune_dataset()
