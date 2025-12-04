# Copyright 2025 AtomArtist. All rights reserved.

import logging
from flask import Flask
from flask_sqlalchemy import SQLAlchemy
from flask_migrate import Migrate

from config import Config

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

db = SQLAlchemy()
migrate = Migrate()


def create_app():
    app = Flask(__name__)
    config = Config.from_env()

    app.config['SQLALCHEMY_DATABASE_URI'] = config.database_url
    app.config['SQLALCHEMY_TRACK_MODIFICATIONS'] = False
    app.config['ENVIRONMENT'] = config.environment

    db.init_app(app)
    migrate.init_app(app, db)

    # Import and register blueprints
    from routes import main_bp
    from routes.api import api_bp
    from routes.health import health_bp

    app.register_blueprint(main_bp)
    app.register_blueprint(api_bp, url_prefix='/api')
    app.register_blueprint(health_bp)

    return app


if __name__ == '__main__':
    app = create_app()
    logger.info("AtomArtist listening on port 8080")
    app.run(host='0.0.0.0', port=8080, debug=False)

