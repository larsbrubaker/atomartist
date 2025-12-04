# Copyright 2025 AtomArtist. All rights reserved.

import os
from dataclasses import dataclass
from dotenv import load_dotenv

load_dotenv()


@dataclass
class Config:
    database_url: str
    environment: str

    @classmethod
    def from_env(cls) -> 'Config':
        database_url = os.environ.get('DATABASE_URL')
        if not database_url:
            raise ValueError("DATABASE_URL must be set")

        environment = os.environ.get('ENVIRONMENT', 'development')

        return cls(database_url=database_url, environment=environment)

