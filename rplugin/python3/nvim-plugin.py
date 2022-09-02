import os
from typing import Optional

import pynvim
import offline_pr_review


@pynvim.plugin
class TestPlugin(object):
    review_active: bool
    review: Optional[offline_pr_review.Review]

    def __init__(self, nvim):
        self.review_active = False
        self.nvim = nvim
        remote_info = self.nvim.call('FugitiveRemote')
        offline_pr_review.update_configuration(remote_info['path'].replace('.git', ''))

    @pynvim.command('StartReview', nargs=1)
    def start_review(self, args):
        self.review = offline_pr_review.new_blank_review(args[0])
        self.review_active = True

    @pynvim.command('PublishReview')
    def publish_review(self):
        self.review_active = False
        self.review.publish(os.getenv("GH_API_TOKEN"))

    @pynvim.function('IsReviewActive', sync=True)
    def is_review_active(self):
        return self.review_active

    @pynvim.command('TestCommand', nargs='*', range='')
    def testcommand(self, args, range):
        self.nvim.current.line = ('Command with args: {}, range: {}'
                                  .format(args, range))
