# Copyright 2025 AtomArtist. All rights reserved.

from flask import Blueprint, jsonify

main_bp = Blueprint('main', __name__)


@main_bp.route('/')
def index():
    return jsonify({
        'message': 'Welcome to AtomArtist!',
        'version': '0.1.0'
    })

