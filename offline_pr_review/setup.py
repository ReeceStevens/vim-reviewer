from setuptools import setup, find_packages

setup(
    name='offline-pr-review',
    version='0.0.1',
    description='An offline interface for performing PR reviews',
    author='Reece Stevens',
    packages=find_packages(),
    install_requires=[
        'requests',
    ],
    python_requires='>= 3.7',
)
