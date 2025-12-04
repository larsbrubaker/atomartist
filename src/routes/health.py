# Copyright 2025 AtomArtist. All rights reserved.

from flask import Blueprint, jsonify

health_bp = Blueprint('health', __name__)


@health_bp.route('/health')
def health_check():
    return jsonify({
        'status': 'healthy',
        'service': 'atomartist'
    })

