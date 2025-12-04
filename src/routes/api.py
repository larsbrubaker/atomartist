# Copyright 2025 AtomArtist. All rights reserved.

from flask import Blueprint, jsonify

api_bp = Blueprint('api', __name__)


@api_bp.route('/')
def api_index():
    return jsonify({
        'message': 'AtomArtist API',
        'version': 'v1'
    })

