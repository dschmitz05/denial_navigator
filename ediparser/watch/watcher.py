"""Simple file watcher for EDI 835 dropzone"""

import asyncio
import os
import threading
import time
import logging
from pathlib import Path
from typing import Callable, Optional

logger = logging.getLogger("ediparser.watcher")


class FileWatcher:
    """Poll-based file watcher for the EDI dropzone directory."""

    def __init__(self, directory: str, callback: Callable, poll_interval: float = 2.0):
        self.directory = os.path.abspath(directory)
        self.callback = callback
        self.poll_interval = poll_interval
        self._running = False
        self._processed = set()
        self._event_loop = asyncio.new_event_loop()
        self._thread = None

    def _run(self):
        """Run the polling loop in a thread."""
        asyncio.set_event_loop(self._event_loop)
        while self._running:
            try:
                for fname in os.listdir(self.directory):
                    fpath = Path(self.directory) / fname
                    if fpath.is_file() and fname not in self._processed:
                        logger.info(f"New file detected: {fname}")
                        try:
                            coro = self.callback(fpath)
                            if asyncio.iscoroutine(coro):
                                self._event_loop.run_until_complete(coro)
                            else:
                                pass  # sync callback
                        except Exception as e:
                            logger.error(f"Error processing {fname}: {e}")
                        self._processed.add(fname)
            except Exception as e:
                logger.error(f"Watch error: {e}")
            time.sleep(self.poll_interval)

    def mark_processed(self, file_name: str):
        """Claim a filename so the poller does not process it a second time.

        The /ingest endpoint parses its upload inline and also drops a copy in
        the dropzone for audit; without this the watcher would pick that copy
        up and parse the same file again.
        """
        self._processed.add(file_name)

    def seed_from_outputs(self, output_dir: str):
        """Treat files that already have parsed output as done.

        `_processed` lives in memory, so without this every restart reparses
        and re-stores the entire dropzone history. That is how the ingestion
        log reached 91 entries for a handful of real uploads.
        """
        try:
            existing = {name[:-5] for name in os.listdir(output_dir) if name.endswith(".json")}
        except OSError:
            return
        for fname in existing:
            self._processed.add(fname)
        logger.info(f"Seeded watcher with {len(existing)} already-parsed files")

    def start(self):
        """Start watching the directory."""
        self._running = True
        os.makedirs(self.directory, exist_ok=True)
        logger.info(f"Watching directory: {self.directory}")
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def stop(self):
        """Stop watching."""
        self._running = False
        self._processed.clear()
        if self._thread:
            self._thread.join(timeout=5)
        logger.info("File watcher stopped")
