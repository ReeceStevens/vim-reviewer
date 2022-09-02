import pynvim


@pynvim.plugin
class TestPlugin(object):
    review_active: bool

    def __init__(self, nvim):
        self.review_active = False
        self.nvim = nvim

    @pynvim.command('StartReview')
    def start_review(self):
        self.review_active = True

    @pynvim.function('IsReviewActive', sync=True)
    def is_review_active(self):
        return self.review_active

    @pynvim.command('TestCommand', nargs='*', range='')
    def testcommand(self, args, range):
        self.nvim.current.line = ('Command with args: {}, range: {}'
                                  .format(args, range))
